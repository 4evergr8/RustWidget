use windows::core::PCWSTR;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{LoadIconW, HICON};

/// 应用图标在 exe 资源里的 ID（assets/icon.rc 里定义成 1）。
const APP_ICON_ID: usize = 1;

/// 从 exe 内嵌资源加载应用图标。加载不到返回 None，调用方自己退回系统默认图标。
pub fn app_icon() -> Option<HICON> {
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        LoadIconW(Some(HINSTANCE(hinst.0)), PCWSTR(APP_ICON_ID as *const u16)).ok()
    }
}
