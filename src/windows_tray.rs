//! Optional notification-area lifecycle for the main Windows window.

use echo_gpui::backend;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    DefSubclassProc, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIM_SETFOCUS, NIM_SETVERSION, NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    RemoveWindowSubclass, SetWindowSubclass, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const CALLBACK_CLASS: PCWSTR = w!("Echo.Tray.Callback.v1");
const CALLBACK_MESSAGE: u32 = WM_APP + 0x45;
const SUBCLASS_ID: usize = 0x4543_5452;
const ICON_ID: u32 = 1;
const TIMER_ID: usize = 1;
const SHOW_COMMAND: usize = 1;
const QUIT_COMMAND: usize = 2;

thread_local! {
    static TRAY: RefCell<Option<Box<TrayState>>> = const { RefCell::new(None) };
}

struct TrayState {
    main: HWND,
    callback: Cell<HWND>,
    attached: Cell<bool>,
    added: Cell<bool>,
    version_four: Cell<bool>,
    hidden: Cell<bool>,
    quitting: Cell<bool>,
    menu_open: Cell<bool>,
    unread_count: Cell<usize>,
    retry_after: Cell<Option<Instant>>,
    taskbar_created: u32,
}

pub fn install(window: &gpui::Window) {
    if echo_gpui::benchmark_profile::is_enabled() {
        return;
    }
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let main = HWND(handle.hwnd.get() as *mut _);
    TRAY.with(|tray| {
        if tray.borrow().is_some() {
            return;
        }
        let state = Box::new(TrayState {
            main,
            callback: Cell::new(HWND::default()),
            attached: Cell::new(false),
            added: Cell::new(false),
            version_four: Cell::new(false),
            hidden: Cell::new(false),
            quitting: Cell::new(false),
            menu_open: Cell::new(false),
            unread_count: Cell::new(usize::MAX),
            retry_after: Cell::new(None),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        });
        // All callbacks run on this UI thread; the Box lives until shutdown.
        unsafe {
            let instance = HINSTANCE(GetWindowLongPtrW(main, GWLP_HINSTANCE) as *mut _);
            let class = WNDCLASSW {
                lpfnWndProc: Some(callback_proc),
                hInstance: instance,
                lpszClassName: CALLBACK_CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);
            let pointer = &*state as *const TrayState;
            let callback = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CALLBACK_CLASS,
                w!("Echo tray"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance),
                Some(pointer.cast()),
            ) {
                Ok(callback) => callback,
                Err(error) => {
                    echo_gpui::log_app!(warn, "Could not create tray callback window: {error}");
                    return;
                }
            };
            state.callback.set(callback);
            if !SetWindowSubclass(main, Some(main_proc), SUBCLASS_ID, pointer as usize).as_bool() {
                let _ = DestroyWindow(callback);
                echo_gpui::log_app!(warn, "Could not install tray window handler");
                return;
            }
            state.attached.set(true);
            if SetTimer(Some(callback), TIMER_ID, 500, None) == 0 {
                let _ = RemoveWindowSubclass(main, Some(main_proc), SUBCLASS_ID);
                let _ = DestroyWindow(callback);
                echo_gpui::log_app!(warn, "Could not start tray settings observer");
                return;
            }
            state.sync();
        }
        *tray.borrow_mut() = Some(state);
    });
}

pub fn shutdown() {
    TRAY.with(|tray| {
        if let Some(state) = tray.borrow_mut().take() {
            // Destroy callbacks while their state is still owned by this Box.
            unsafe {
                state.remove_icon();
                if state.attached.replace(false) {
                    let _ = RemoveWindowSubclass(state.main, Some(main_proc), SUBCLASS_ID);
                }
                let callback = state.callback.get();
                if !callback.is_invalid() {
                    let _ = KillTimer(Some(callback), TIMER_ID);
                    let _ = DestroyWindow(callback);
                }
            }
        }
    });
}

impl TrayState {
    fn enabled(&self) -> bool {
        backend::settings::get_minimize_to_tray() || backend::settings::get_close_to_tray()
    }

    fn icon_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.callback.get(),
            uID: ICON_ID,
            ..Default::default()
        }
    }

    unsafe fn sync(&self) {
        unsafe {
            if !self.attached.get() || self.quitting.get() {
                return;
            }
            if !self.enabled() {
                self.remove_icon();
                if self.hidden.replace(false) {
                    let command = if IsIconic(self.main).as_bool() {
                        SW_SHOWMINNOACTIVE
                    } else {
                        SW_SHOWNA
                    };
                    let _ = ShowWindow(self.main, command);
                }
                return;
            }
            if self
                .retry_after
                .get()
                .is_some_and(|deadline| Instant::now() < deadline)
            {
                return;
            }
            let unread = backend::get_unread_global_mention_count();
            if self.added.get() && self.unread_count.get() == unread {
                return;
            }
            let mut data = self.icon_data();
            data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
            data.uCallbackMessage = CALLBACK_MESSAGE;
            let class_icon = GetClassLongPtrW(self.main, GCLP_HICONSM);
            let class_icon = if class_icon == 0 {
                GetClassLongPtrW(self.main, GCLP_HICON)
            } else {
                class_icon
            };
            data.hIcon = if class_icon == 0 {
                LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
            } else {
                HICON(class_icon as *mut _)
            };
            let tooltip = if unread == 0 {
                "Echo".to_string()
            } else {
                format!("Echo — {unread} unread mentions")
            };
            for (target, character) in data.szTip.iter_mut().take(127).zip(tooltip.encode_utf16()) {
                *target = character;
            }
            let adding = !self.added.get();
            if Shell_NotifyIconW(if adding { NIM_ADD } else { NIM_MODIFY }, &data).as_bool() {
                self.added.set(true);
                self.unread_count.set(unread);
                self.retry_after.set(None);
                if adding {
                    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                    self.version_four
                        .set(Shell_NotifyIconW(NIM_SETVERSION, &data).as_bool());
                }
            } else {
                self.remove_icon();
                self.retry_after
                    .set(Some(Instant::now() + Duration::from_secs(5)));
            }
        }
    }

    unsafe fn remove_icon(&self) {
        if self.added.replace(false) || self.unread_count.get() != usize::MAX {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &self.icon_data());
            }
        }
        self.unread_count.set(usize::MAX);
    }

    unsafe fn hide(&self) -> bool {
        unsafe {
            self.sync();
            // Never strand a hidden main window when the shell rejected the icon.
            if !self.added.get() {
                return false;
            }
            self.hidden.set(true);
            let _ = ShowWindow(self.main, SW_HIDE);
            true
        }
    }

    unsafe fn show(&self) {
        unsafe {
            self.hidden.set(false);
            let _ = ShowWindow(
                self.main,
                if IsIconic(self.main).as_bool() {
                    SW_RESTORE
                } else {
                    SW_SHOW
                },
            );
            let _ = SetForegroundWindow(self.main);
        }
    }

    unsafe fn menu(&self) {
        if self.menu_open.replace(true) {
            return;
        }
        unsafe {
            if let Ok(menu) = CreatePopupMenu() {
                let _ = AppendMenuW(menu, MF_STRING, SHOW_COMMAND, w!("Show Echo"));
                let _ = AppendMenuW(menu, MF_STRING, QUIT_COMMAND, w!("Quit"));
                let mut position = POINT::default();
                if GetCursorPos(&mut position).is_ok() {
                    let _ = SetForegroundWindow(self.main);
                    let command = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                        position.x,
                        position.y,
                        None,
                        self.main,
                        None,
                    )
                    .0 as usize;
                    let _ = PostMessageW(Some(self.main), WM_NULL, WPARAM(0), LPARAM(0));
                    match command {
                        SHOW_COMMAND => self.show(),
                        QUIT_COMMAND => {
                            self.quitting.set(true);
                            if PostMessageW(Some(self.main), WM_CLOSE, WPARAM(0), LPARAM(0))
                                .is_err()
                            {
                                self.quitting.set(false);
                            }
                        }
                        _ => {}
                    }
                }
                let _ = DestroyMenu(menu);
                let _ = Shell_NotifyIconW(NIM_SETFOCUS, &self.icon_data());
            }
        }
        self.menu_open.set(false);
    }
}

unsafe extern "system" fn callback_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayState;
        if let Some(state) = pointer.as_ref() {
            if message == WM_TIMER && wparam.0 == TIMER_ID {
                state.sync();
                return LRESULT(0);
            }
            if message == CALLBACK_MESSAGE {
                let event = lparam.0 as u32 & 0xffff;
                if matches!(event, WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_SELECT)
                    || event == (NIN_SELECT | NINF_KEY)
                {
                    state.show();
                } else if event == WM_CONTEXTMENU
                    || (!state.version_four.get() && event == WM_RBUTTONUP)
                {
                    state.menu();
                }
                return LRESULT(0);
            }
            if message == WM_NCDESTROY {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                state.callback.set(HWND::default());
            }
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

unsafe extern "system" fn main_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let state = &*(data as *const TrayState);
        if message == state.taskbar_created && message != 0 {
            state.added.set(false);
            state.retry_after.set(None);
            state.sync();
        }
        if !state.quitting.get() {
            if message == WM_CLOSE && backend::settings::get_close_to_tray() && state.hide() {
                return LRESULT(0);
            }
            if message == WM_SYSCOMMAND
                && wparam.0 as u32 & 0xfff0 == SC_MINIMIZE
                && backend::settings::get_minimize_to_tray()
                && state.hide()
            {
                return LRESULT(0);
            }
            if message == WM_SIZE
                && wparam.0 as u32 == SIZE_MINIMIZED
                && backend::settings::get_minimize_to_tray()
            {
                state.hide();
            }
        }
        if message == WM_SHOWWINDOW && wparam.0 != 0 {
            state.hidden.set(false);
        }
        if message == WM_NCDESTROY {
            state.attached.set(false);
            state.remove_icon();
            let _ = KillTimer(Some(state.callback.get()), TIMER_ID);
            let _ = RemoveWindowSubclass(hwnd, Some(main_proc), subclass);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}
