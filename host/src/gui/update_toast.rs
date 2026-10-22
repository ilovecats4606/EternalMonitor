//! One Windows notification per new version. A notification-area icon with a
//! balloon works for an unpackaged app (toasts need a registered app ID), and
//! Windows 10 and 11 show the balloon as a regular notification. Clicking it,
//! or the icon, brings the window forward, where the banner is waiting.

use eframe::egui;

#[cfg(windows)]
pub fn show(ctx: &egui::Context, version: &str) {
    imp::show(ctx, version);
}

#[cfg(not(windows))]
pub fn show(_ctx: &egui::Context, version: &str) {
    tracing::info!(%version, "Update available (notifications are Windows-only)");
}

/// Removes the icon. Safe to call when none is shown.
pub fn hide() {
    #[cfg(windows)]
    imp::hide();
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicIsize, Ordering};

    use eframe::egui;
    use parking_lot::Mutex;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_LARGE_ICON,
        NIIF_RESPECT_QUIET_TIME, NIIF_USER, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
        NIN_BALLOONUSERCLICK, NIN_SELECT, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, LoadIconW,
        PostMessageW, PostQuitMessage, RegisterClassW, TranslateMessage, IDI_APPLICATION, MSG,
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_DESTROY, WM_LBUTTONUP, WNDCLASSW,
    };

    const CALLBACK: u32 = WM_APP + 1;
    /// `NIN_SELECT | NINF_KEY`: Enter or Space on the icon.
    const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
    const ICON_ID: u32 = 1;
    static CONTEXT: Mutex<Option<egui::Context>> = parking_lot::const_mutex(None);
    static WINDOW: AtomicIsize = AtomicIsize::new(0);

    pub fn show(ctx: &egui::Context, version: &str) {
        hide();
        *CONTEXT.lock() = Some(ctx.clone());
        let title = format!("EternalMonitor {version} is available");
        let spawned = std::thread::Builder::new()
            .name("update-notice".into())
            .spawn(move || {
                if let Err(error) = unsafe { run(&title) } {
                    tracing::warn!(%error, "Could not show the update notification");
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "Could not start the update notification");
        }
    }

    fn bring_forward(ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    pub fn hide() {
        let hwnd = WINDOW.swap(0, Ordering::SeqCst);
        if hwnd != 0 {
            let hwnd = HWND(hwnd as _);
            unsafe {
                // Delete synchronously: the process may exit before the
                // notification thread runs again.
                let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            ..Default::default()
        }
    }

    fn copy(target: &mut [u16], text: &str) {
        let wide: Vec<u16> = text.encode_utf16().take(target.len() - 1).collect();
        target[..wide.len()].copy_from_slice(&wide);
        target[wide.len()] = 0;
    }

    unsafe fn run(title: &str) -> windows::core::Result<()> {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("EternalMonitorUpdateNotice");
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        };
        // Registering twice fails harmlessly when a second version arrives.
        RegisterClassW(&window_class);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("EternalMonitor update"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            // A hidden top-level window: the shell does not show balloons
            // reliably for message-only windows.
            None,
            None,
            instance,
            None,
        )?;
        // winresource embeds the app icon as resource 1.
        let icon =
            LoadIconW(instance, PCWSTR(1 as _)).or_else(|_| LoadIconW(None, IDI_APPLICATION))?;
        // Add the icon and set its version first; the balloon is a separate
        // modify, which the shell shows more reliably than one sent with NIM_ADD.
        let mut data = icon_data(hwnd);
        data.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
        data.uCallbackMessage = CALLBACK;
        data.hIcon = icon;
        copy(&mut data.szTip, "EternalMonitor update available");
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
            let _ = DestroyWindow(hwnd);
            return Err(windows::core::Error::from_win32());
        }
        WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        data.uFlags = NIF_INFO;
        data.hBalloonIcon = icon;
        data.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON | NIIF_RESPECT_QUIET_TIME;
        copy(&mut data.szInfoTitle, title);
        copy(&mut data.szInfo, "Click to open EternalMonitor and update.");
        let shown = Shell_NotifyIconW(NIM_MODIFY, &data).as_bool();
        tracing::info!(shown, "Update notification");
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        Ok(())
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            CALLBACK => {
                // NOTIFYICON_VERSION_4: the event is the low word of lParam.
                let event = (lparam.0 & 0xFFFF) as u32;
                if [
                    NIN_BALLOONUSERCLICK,
                    NIN_SELECT,
                    NIN_KEYSELECT,
                    WM_LBUTTONUP,
                ]
                .contains(&event)
                {
                    if let Some(ctx) = CONTEXT.lock().clone() {
                        bring_forward(&ctx);
                    }
                    hide();
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
                let _ =
                    WINDOW.compare_exchange(hwnd.0 as isize, 0, Ordering::SeqCst, Ordering::SeqCst);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}
