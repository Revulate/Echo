param([string]$ForkPath = (Join-Path $PSScriptRoot '..\..\zed-gpui-fork'))

$ErrorActionPreference = 'Stop'
$source = [IO.File]::ReadAllText((Join-Path $ForkPath 'crates/gpui_windows/src/events.rs'))
$changed = [regex]::Match($source, '(?s)    fn handle_cursor_changed\(.*?(?=    fn owns_cursor\()').Value
$setCursor = [regex]::Match($source, '(?s)    fn handle_set_cursor\(.*?(?=    fn handle_system_settings_changed\()').Value
if (-not $changed -or -not $setCursor) { throw 'Could not extract the pinned cursor handlers' }

# Compile the actual handler bodies with deterministic Win32 boundaries. This
# tests state transitions without moving the pointer or opening native windows.
$harness = @'
#![allow(non_snake_case, non_camel_case_types)]
use std::{cell::Cell, sync::atomic::{AtomicBool, Ordering}};
type HWND = usize;
#[derive(Clone, Copy, Debug, PartialEq)]
struct HCURSOR(isize);
struct LPARAM(isize);
impl LPARAM { fn loword(&self) -> u16 { self.0 as u16 } }
struct WinBool(bool);
impl WinBool { fn as_bool(&self) -> bool { self.0 } }
const HTLEFT: u32 = 10; const HTRIGHT: u32 = 11; const HTTOP: u32 = 12;
const HTTOPLEFT: u32 = 13; const HTTOPRIGHT: u32 = 14; const HTBOTTOM: u32 = 15;
const HTBOTTOMLEFT: u32 = 16; const HTBOTTOMRIGHT: u32 = 17;
thread_local! {
    static OWNS: Cell<bool> = const { Cell::new(true) };
    static SHOWN: Cell<Option<HCURSOR>> = const { Cell::new(Some(HCURSOR(1))) };
}
unsafe fn SetCursor(cursor: Option<HCURSOR>) { SHOWN.with(|shown| shown.set(cursor)); }
unsafe fn IsWindowEnabled(_: HWND) -> WinBool { WinBool(true) }
struct State { current_cursor: Cell<Option<HCURSOR>>, cursor_visible: AtomicBool }
struct WindowsWindowInner { state: State }
impl WindowsWindowInner {
    fn owns_cursor(_: HWND) -> bool { OWNS.with(Cell::get) }
'@
$harness += $changed + $setCursor + @'
}
fn main() {
    let window = WindowsWindowInner { state: State {
        current_cursor: Cell::new(Some(HCURSOR(1))), cursor_visible: AtomicBool::new(true)
    }};
    // Arrow -> I-beam -> hand must survive subsequent WM_SETCURSOR events.
    for style in [2, 3, 1, 3, 2] {
        window.handle_cursor_changed(1, LPARAM(style));
        assert_eq!(window.state.current_cursor.get(), Some(HCURSOR(style)));
        window.handle_set_cursor(1, LPARAM(1));
        assert_eq!(SHOWN.with(Cell::get), Some(HCURSOR(style)));
    }
    // Cache changes even while another window owns the pointer, without
    // publishing into that other window's cursor state.
    OWNS.with(|owns| owns.set(false));
    window.handle_cursor_changed(1, LPARAM(3));
    assert_eq!(window.state.current_cursor.get(), Some(HCURSOR(3)));
    assert_eq!(SHOWN.with(Cell::get), Some(HCURSOR(2)));
    OWNS.with(|owns| owns.set(true));
    window.handle_set_cursor(1, LPARAM(1));
    assert_eq!(SHOWN.with(Cell::get), Some(HCURSOR(3)));
    // Hiding the cursor must not erase the style restored on mouse movement.
    window.state.cursor_visible.store(false, Ordering::Relaxed);
    window.handle_cursor_changed(1, LPARAM(2));
    assert_eq!(SHOWN.with(Cell::get), None);
    window.state.cursor_visible.store(true, Ordering::Relaxed);
    window.handle_set_cursor(1, LPARAM(1));
    assert_eq!(SHOWN.with(Cell::get), Some(HCURSOR(2)));
    assert_eq!(window.handle_set_cursor(1, LPARAM(HTLEFT as isize)), None);
    assert_eq!(SHOWN.with(Cell::get), Some(HCURSOR(2)));
    println!("PASS: cursor persistence, owner isolation, hide/restore, and native resize deferral");
}
'@
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('echo-cursor-check-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $testRoot
$testSource = Join-Path $testRoot 'cursor_check.rs'
$testBinary = Join-Path $testRoot 'cursor_check.exe'
[IO.File]::WriteAllText($testSource, $harness, [Text.UTF8Encoding]::new($false))
& rustc --edition 2024 $testSource -o $testBinary
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& $testBinary
exit $LASTEXITCODE
