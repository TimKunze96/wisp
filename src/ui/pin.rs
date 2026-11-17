//! Keeping a window above all others. macOS and Windows let an app do that
//! for its own windows; Wayland leaves it to the compositor (on Hyprland: a
//! "pin" rule or keybind).

use gpui_kit::Window;

/// Whether this platform lets us pin.
pub const AVAILABLE: bool = cfg!(any(target_os = "macos", windows));

/// Keep `window` on top of everything (or not any more).
pub fn set(window: &Window, on: bool) {
    #[cfg(any(target_os = "macos", windows))]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
        match handle.as_raw() {
            #[cfg(windows)]
            RawWindowHandle::Win32(h) => unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
                };
                let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
                SetWindowPos(h.hwnd.get() as _, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            },
            #[cfg(target_os = "macos")]
            RawWindowHandle::AppKit(h) => unsafe {
                use objc2_app_kit::{NSFloatingWindowLevel, NSNormalWindowLevel, NSView};
                let view: &NSView = h.ns_view.cast::<NSView>().as_ref();
                if let Some(w) = view.window() {
                    w.setLevel(if on { NSFloatingWindowLevel } else { NSNormalWindowLevel });
                }
            },
            _ => {}
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (window, on);
}
