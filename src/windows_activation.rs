use echo_gpui::backend;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnumWindows, GetPropW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, PostMessageW, RegisterWindowMessageW, RemovePropW, SW_RESTORE, SW_SHOW,
    SetForegroundWindow, SetPropW, ShowWindow, WM_NCDESTROY,
};
use windows::core::{BOOL, PCWSTR, w};

const WINDOW_PROPERTY: PCWSTR = w!("Echo.MainWindow.Activation.v1");
const ACTIVATION_MESSAGE: PCWSTR = w!("Echo.ActivateMainWindow.v1");
const SUBCLASS_ID: usize = 0x4543_484f;

pub fn acquire_single_instance() -> bool {
    let mutex_name = std::ffi::OsStr::new(backend::browser_watching::GUI_INSTANCE_MUTEX_NAME)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: the name is terminated and the owning handle is retained for process lifetime.
    unsafe {
        let Ok(handle) = CreateMutexW(None, true, PCWSTR(mutex_name.as_ptr())) else {
            return true;
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            if !echo_gpui::benchmark_profile::is_enabled() {
                activate_existing();
            }
            return false;
        }
    }
    true
}

pub fn install(window: &gpui::Window) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    // SAFETY: this runs on the existing window's owning UI thread; the callback is static.
    unsafe {
        let message = RegisterWindowMessageW(ACTIVATION_MESSAGE);
        if message == 0
            || !SetWindowSubclass(hwnd, Some(activation_proc), SUBCLASS_ID, message as usize)
                .as_bool()
        {
            echo_gpui::log_app!(warn, "Could not install main-window activation handler");
            return;
        }
        let owner = HANDLE(std::process::id() as usize as *mut _);
        if let Err(error) = SetPropW(hwnd, WINDOW_PROPERTY, Some(owner)) {
            let _ = RemoveWindowSubclass(hwnd, Some(activation_proc), SUBCLASS_ID);
            echo_gpui::log_app!(warn, "Could not register main-window activation: {error}");
        }
    }
}

unsafe extern "system" fn activation_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    activation_message: usize,
) -> LRESULT {
    // SAFETY: the subclass receives its own live HWND and forwards all other messages.
    unsafe {
        if message == activation_message as u32 {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            } else if !IsWindowVisible(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            let _ = SetForegroundWindow(hwnd);
            return LRESULT(0);
        }
        if message == WM_NCDESTROY {
            let _ = RemovePropW(hwnd, WINDOW_PROPERTY);
            let _ = RemoveWindowSubclass(hwnd, Some(activation_proc), subclass_id);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

struct WindowSearch {
    target: Option<(HWND, u32)>,
}

unsafe extern "system" fn find_main_window(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: EnumWindows is synchronous and data points to the caller's WindowSearch.
    unsafe {
        let search = &mut *(data.0 as *mut WindowSearch);
        let owner = GetPropW(hwnd, WINDOW_PROPERTY).0 as usize as u32;
        if owner == 0 || owner == std::process::id() {
            return BOOL(1);
        }
        let mut process_id = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        // Only Echo sets this property, and only on its own main window.
        if process_id == owner {
            search.target = Some((hwnd, process_id));
            return BOOL(0);
        }
        BOOL(1)
    }
}

fn activate_existing() {
    let mut search = WindowSearch { target: None };
    let deadline = Instant::now() + Duration::from_secs(5);
    // A second launch can arrive before the first has constructed its main HWND.
    loop {
        // SAFETY: search outlives the synchronous enumeration and only marked Echo HWNDs qualify.
        unsafe {
            let _ = EnumWindows(
                Some(find_main_window),
                LPARAM(&mut search as *mut _ as isize),
            );
            if let Some((hwnd, process_id)) = search.target {
                let mut current_owner = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut current_owner));
                if current_owner == process_id
                    && GetPropW(hwnd, WINDOW_PROPERTY).0 as usize == process_id as usize
                {
                    let message = RegisterWindowMessageW(ACTIVATION_MESSAGE);
                    if message != 0 {
                        let _ = AllowSetForegroundWindow(process_id);
                        if let Err(error) = PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0))
                        {
                            eprintln!("Could not activate the running Echo window: {error}");
                        }
                    }
                    return;
                }
                search.target = None;
            }
        }
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
