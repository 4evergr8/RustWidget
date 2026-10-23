use std::mem::size_of;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorInfo, LoadCursorW, CURSORINFO, CURSOR_SHOWING, IDC_APPSTARTING, IDC_ARROW, IDC_CROSS,
    IDC_HAND, IDC_HELP, IDC_IBEAM, IDC_NO, IDC_PERSON, IDC_PIN, IDC_SIZEALL, IDC_SIZENESW,
    IDC_SIZENWSE, IDC_SIZENS, IDC_SIZEWE, IDC_UPARROW, IDC_WAIT,
};

/// 手写光标。windows-rs 未导出 IDC_PEN，按 Win32 定义补上。
const IDC_PEN: PCWSTR = PCWSTR(32631u16 as _);

/// 与素材目录一一对应的光标状态。
///
/// 目录名沿用 Windows「鼠标指针」方案列表里的官方中文名，不带任何后缀。
/// 共 17 个角色，与系统方案编辑器里的条目顺序一致。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CursorState {
    Normal,
    Help,
    Working,
    Busy,
    Precise,
    Text,
    Handwriting,
    Unavailable,
    SizeNS,
    SizeWE,
    SizeNWSE,
    SizeNESW,
    Move,
    Alternate,
    Link,
    Pin,
    Person,
}

impl CursorState {
    pub const ALL: [CursorState; 17] = [
        CursorState::Normal,
        CursorState::Help,
        CursorState::Working,
        CursorState::Busy,
        CursorState::Precise,
        CursorState::Text,
        CursorState::Handwriting,
        CursorState::Unavailable,
        CursorState::SizeNS,
        CursorState::SizeWE,
        CursorState::SizeNWSE,
        CursorState::SizeNESW,
        CursorState::Move,
        CursorState::Alternate,
        CursorState::Link,
        CursorState::Pin,
        CursorState::Person,
    ];

    /// 去重后的目录列表。目前 17 个状态各有一个独立目录，保留这个函数是为了
    /// 将来若又出现共用素材的情况不用改调用方。
    pub fn unique_dirs() -> Vec<&'static str> {
        let mut v: Vec<&'static str> = Vec::new();
        for st in Self::ALL {
            let d = st.dir();
            if !v.contains(&d) {
                v.push(d);
            }
        }
        v
    }

    /// 素材子目录名，与 Windows 指针方案列表里的中文名一致。
    pub fn dir(&self) -> &'static str {
        match self {
            CursorState::Normal => "正常选择",
            CursorState::Help => "帮助选择",
            CursorState::Working => "后台运行",
            CursorState::Busy => "忙",
            CursorState::Precise => "精确选择",
            CursorState::Text => "文本选择",
            CursorState::Handwriting => "手写",
            CursorState::Unavailable => "不可用",
            CursorState::SizeNS => "垂直调整大小",
            CursorState::SizeWE => "水平调整大小",
            CursorState::SizeNWSE => "沿对角线调整大小 1",
            CursorState::SizeNESW => "沿对角线调整大小 2",
            CursorState::Move => "移动",
            CursorState::Alternate => "候选",
            CursorState::Link => "链接选择",
            CursorState::Pin => "位置选择",
            CursorState::Person => "个人选择",
        }
    }
}

/// 光标状态检测。
///
/// 系统光标是固定槽位，句柄在会话内恒定，换主题只替换槽位里的图像而不换句柄，
/// 所以直接比对 `LoadCursorW` 拿到的共享句柄即可，不需要读图像算哈希。
/// 唯一检测不到的是程序自己 `LoadCursorFromFile` 设置的光标（句柄不在槽位表里），
/// 这类统一落到 Normal。
pub struct CursorWatcher {
    table: Vec<(CursorState, isize)>,
    stable: CursorState,
    candidate: Option<(CursorState, Instant)>,
}

const DEBOUNCE: Duration = Duration::from_millis(120);

impl CursorWatcher {
    pub fn new() -> Self {
        let ids = [
            (CursorState::Normal, IDC_ARROW),
            (CursorState::Help, IDC_HELP),
            (CursorState::Working, IDC_APPSTARTING),
            (CursorState::Busy, IDC_WAIT),
            (CursorState::Precise, IDC_CROSS),
            (CursorState::Text, IDC_IBEAM),
            (CursorState::Handwriting, IDC_PEN),
            (CursorState::Unavailable, IDC_NO),
            (CursorState::SizeNS, IDC_SIZENS),
            (CursorState::SizeWE, IDC_SIZEWE),
            (CursorState::SizeNWSE, IDC_SIZENWSE),
            (CursorState::SizeNESW, IDC_SIZENESW),
            (CursorState::Move, IDC_SIZEALL),
            (CursorState::Alternate, IDC_UPARROW),
            (CursorState::Link, IDC_HAND),
            (CursorState::Pin, IDC_PIN),
            (CursorState::Person, IDC_PERSON),
        ];

        let mut table = Vec::with_capacity(ids.len());
        unsafe {
            for (state, id) in ids {
                if let Ok(h) = LoadCursorW(None, id) {
                    table.push((state, h.0 as isize));
                }
            }
        }

        Self {
            table,
            stable: CursorState::Normal,
            candidate: None,
        }
    }

    /// 轮询并返回稳定后的状态。切换需要持续 120ms，避免鼠标划过链接时来回抖动。
    pub fn poll(&mut self, now: Instant) -> CursorState {
        let detected = unsafe {
            let mut ci = CURSORINFO {
                cbSize: size_of::<CURSORINFO>() as u32,
                ..Default::default()
            };
            if GetCursorInfo(&mut ci).is_err() || (ci.flags.0 & CURSOR_SHOWING.0) == 0 {
                // 光标被隐藏时维持上一状态
                self.stable
            } else {
                let raw = ci.hCursor.0 as isize;
                self.table
                    .iter()
                    .find(|(_, h)| *h == raw)
                    .map(|(s, _)| *s)
                    .unwrap_or(CursorState::Normal)
            }
        };

        if detected == self.stable {
            self.candidate = None;
            return self.stable;
        }

        match self.candidate {
            Some((c, since)) if c == detected => {
                if now.duration_since(since) >= DEBOUNCE {
                    self.stable = detected;
                    self.candidate = None;
                }
            }
            _ => self.candidate = Some((detected, now)),
        }

        self.stable
    }
}
