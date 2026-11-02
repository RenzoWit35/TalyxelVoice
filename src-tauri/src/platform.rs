//! OS glue: which app is focused, simulated paste, and an overlay that never
//! takes focus (if it did, the paste would land in the overlay).

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct FocusedApp {
    /// Executable name, e.g. `Code.exe`; drives per-app modes.
    pub exe: String,
    pub title: String,
}

#[cfg(windows)]
mod imp {
    use std::time::{Duration, Instant};

    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
        KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU,
        VK_RWIN, VK_SHIFT, VK_V,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId,
        SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_TOPMOST,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };

    use super::FocusedApp;

    pub fn focused_app() -> FocusedApp {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return FocusedApp::default();
            }
            let mut title = [0u16; 512];
            let n = GetWindowTextW(hwnd, &mut title).max(0) as usize;
            let title = String::from_utf16_lossy(&title[..n]);

            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let mut exe = String::new();
            if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut buf = [0u16; 1024];
                let mut len = buf.len() as u32;
                if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                    let path = String::from_utf16_lossy(&buf[..len as usize]);
                    exe = path.rsplit('\\').next().unwrap_or_default().to_owned();
                }
                let _ = CloseHandle(handle);
            }
            FocusedApp { exe, title }
        }
    }

    fn held(vk: VIRTUAL_KEY) -> bool {
        unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
    }

    /// A still-held Shift or Alt from the hotkey would turn our Ctrl+V into
    /// something else, so wait (briefly) for the user to let go.
    pub fn wait_for_modifiers_released() {
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline
            && [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN].into_iter().any(held)
        {
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    pub fn send_paste() {
        let inputs = [
            key(VK_CONTROL, false),
            key(VK_V, false),
            key(VK_V, true),
            key(VK_CONTROL, true),
        ];
        unsafe {
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }

    pub fn make_overlay(hwnd: *mut std::ffi::c_void) {
        let hwnd = HWND(hwnd);
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                style | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize,
            );
        }
    }

    /// Pairs with `show_without_focus`: tao never saw that show, so it thinks
    /// the window is still hidden and its own `hide()` is a no-op.
    pub fn hide_window(hwnd: *mut std::ffi::c_void) {
        unsafe {
            let _ = ShowWindow(HWND(hwnd), SW_HIDE);
        }
    }

    pub fn show_without_focus(hwnd: *mut std::ffi::c_void) {
        let hwnd = HWND(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::FocusedApp;
    pub fn focused_app() -> FocusedApp {
        FocusedApp::default()
    }
    pub fn wait_for_modifiers_released() {}
    pub fn send_paste() {
        log::warn!("paste simulation not implemented on this platform yet");
    }
    pub fn make_overlay(_: *mut std::ffi::c_void) {}
    pub fn show_without_focus(_: *mut std::ffi::c_void) {}
    pub fn hide_window(_: *mut std::ffi::c_void) {}
}

pub use imp::*;

/// Puts `text` into the focused app via the clipboard, then puts back
/// whatever the user had copied.
pub fn insert_text(text: &str, restore_clipboard: bool) -> anyhow::Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    let previous = if restore_clipboard { clipboard.get_text().ok() } else { None };
    clipboard.set_text(text)?;
    wait_for_modifiers_released();
    send_paste();
    if let Some(prev) = previous {
        // The target app reads the clipboard asynchronously after Ctrl+V.
        std::thread::sleep(std::time::Duration::from_millis(250));
        clipboard.set_text(prev)?;
    }
    Ok(())
}
