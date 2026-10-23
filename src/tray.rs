use std::mem::size_of;

use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{LoadIconW, IDI_APPLICATION, WM_APP};

pub const WM_TRAY: u32 = WM_APP + 1;

const TRAY_ID: u32 = 1;

fn make_nid(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        ..Default::default()
    }
}

/// 在右下角通知区添加图标。单击图标打开设置界面。
pub fn add(hwnd: HWND) -> Result<()> {
    unsafe {
        let mut nid = make_nid(hwnd);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = crate::icon::app_icon()
            .or_else(|| LoadIconW(None, IDI_APPLICATION).ok())
            .unwrap_or_default();

        let tip: Vec<u16> = "RustWidget — 单击打开设置".encode_utf16().collect();
        let n = tip.len().min(nid.szTip.len() - 1);
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        nid.szTip[n] = 0;

        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            return Err(windows::core::Error::from_thread());
        }
    }
    Ok(())
}

pub fn remove(hwnd: HWND) {
    unsafe {
        let nid = make_nid(hwnd);
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}
