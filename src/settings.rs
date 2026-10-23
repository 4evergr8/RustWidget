use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    LineTo, MoveToEx, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, HFONT,
    HGDIOBJ, PAINTSTRUCT, PS_SOLID, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DEFAULT_PITCH, DT_CENTER, DT_SINGLELINE, DT_VCENTER, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    CheckRadioButton, IsDlgButtonChecked, BST_CHECKED, DRAWITEMSTRUCT, ODS_SELECTED, ODT_BUTTON,
    UDM_SETBUDDY, UDM_SETPOS32, UDM_SETRANGE32, UDS_ALIGNRIGHT, UDS_ARROWKEYS, UDS_SETBUDDYINT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{list_skins, Config, Mode};

pub enum Cmd {
    Apply(Config),
    Exit,
}

static CMDS: Mutex<Vec<Cmd>> = Mutex::new(Vec::new());
static CFG: Mutex<Option<Config>> = Mutex::new(None);
static EXE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static HWND_UI: AtomicIsize = AtomicIsize::new(0);
static F_TITLE: AtomicIsize = AtomicIsize::new(0);
static F_BODY: AtomicIsize = AtomicIsize::new(0);
static BRUSH_BG: AtomicIsize = AtomicIsize::new(0);
/// 分组标题的基线 Y，用于在 WM_PAINT 里画分隔线
static SECTIONS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

pub fn take_cmds() -> Vec<Cmd> {
    std::mem::take(&mut *CMDS.lock().unwrap())
}

// ---- 配色 ----
const fn rgb(r: u32, g: u32, b: u32) -> COLORREF {
    COLORREF(r | (g << 8) | (b << 16))
}
const C_BG: COLORREF = rgb(250, 250, 251);
const C_TITLE: COLORREF = rgb(28, 28, 32);
const C_MUTED: COLORREF = rgb(108, 108, 118);
const C_SECTION: COLORREF = rgb(122, 122, 132);
const C_LINE: COLORREF = rgb(228, 228, 234);
const C_ACCENT: COLORREF = rgb(79, 107, 237);
const C_ACCENT_DOWN: COLORREF = rgb(58, 82, 200);
const C_OUTLINE: COLORREF = rgb(214, 214, 221);
const C_OUTLINE_DOWN: COLORREF = rgb(238, 238, 242);
const C_WHITE: COLORREF = rgb(255, 255, 255);

// ---- 控件 ID ----
const ID_TITLE: i32 = 100;
const ID_SUB: i32 = 101;
const ID_SEC_BASE: i32 = 200;
const ID_SKIN: i32 = 1001;
const ID_DISPLAY: i32 = 1010;
const ID_FRAME: i32 = 1011;
const ID_ROPE: i32 = 1012;
const ID_GRAVITY: i32 = 1013;
const ID_DAMPING: i32 = 1014;
const ID_ANGLE: i32 = 1015;
const ID_CLEARANCE: i32 = 1016;
const ID_DOCK_X: i32 = 1017;
const ID_DOCK_Y: i32 = 1018;
const ID_POLL: i32 = 1019;
const ID_PHYS: i32 = 1040;
const ID_APPLY: i32 = 1020;
const ID_QUIT: i32 = 1022;
const ID_MODE_PENDANT: i32 = 1030;
const ID_MODE_DOCK: i32 = 1031;
/// 上下按钮的控件 ID = 对应输入框 ID + 这个偏移
const UP_OFFSET: i32 = 5000;

// ---- 布局 ----
const PAD: i32 = 28;
const LABEL_W: i32 = 118;
const FIELD_X: i32 = 152;
const FIELD_W: i32 = 280;
const ROW_H: i32 = 30;
const FIELD_H: i32 = 24;
const CLIENT_W: i32 = 460;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn make_font(height: i32, weight: i32) -> HFONT {
    let face = wide("Microsoft YaHei UI");
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            PCWSTR(face.as_ptr()),
        )
    }
}

pub fn sync(cfg: &Config, exe_dir: &Path) {
    *CFG.lock().unwrap() = Some(cfg.clone());
    *EXE_DIR.lock().unwrap() = Some(exe_dir.to_path_buf());
}

pub fn show_current() {
    unsafe {
        let existing = HWND_UI.load(Ordering::SeqCst);
        if existing != 0 {
            let hwnd = HWND(existing as *mut _);
            populate(hwnd);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            return;
        }

        let Ok(hmodule) = GetModuleHandleW(None) else {
            return;
        };
        let hinstance = HINSTANCE(hmodule.0);

        let brush = CreateSolidBrush(C_BG);
        BRUSH_BG.store(brush.0 as isize, Ordering::SeqCst);

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(ui_proc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: brush,
            lpszClassName: w!("RustWidgetSettings"),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: CLIENT_W,
            bottom: 400,
        };
        let _ = AdjustWindowRect(&mut rc, style, false);

        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("RustWidgetSettings"),
            w!("RustWidget 设置"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rc.right - rc.left,
            rc.bottom - rc.top,
            None,
            None,
            Some(hinstance),
            None,
        ) else {
            return;
        };

        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &round as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&round) as u32,
        );

        HWND_UI.store(hwnd.0 as isize, Ordering::SeqCst);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// 按内容高度调整窗口尺寸（WM_CREATE 里创建完子控件后调用）。
unsafe fn fit_to_content(hwnd: HWND, client_h: i32) {
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: CLIENT_W,
        bottom: client_h,
    };
    let _ = AdjustWindowRect(&mut rc, style, false);
    let _ = SetWindowPos(
        hwnd,
        None,
        0,
        0,
        rc.right - rc.left,
        rc.bottom - rc.top,
        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
}

unsafe fn create_children(hwnd: HWND) -> i32 {
    let hmodule = GetModuleHandleW(None).unwrap_or_default();
    let hinstance = HINSTANCE(hmodule.0);

    let f_title = make_font(-24, 600);
    let f_body = make_font(-14, 400);
    F_TITLE.store(f_title.0 as isize, Ordering::SeqCst);
    F_BODY.store(f_body.0 as isize, Ordering::SeqCst);

    SECTIONS.lock().unwrap().clear();
    let mut sec_idx = 0;

    let mut y = 18;
    label(hwnd, ID_TITLE, "设置", PAD, y, 300, 32, hinstance, f_title);
    y += 34;
    label(hwnd, ID_SUB, "外观、显示模式与摆动手感", PAD, y, 380, 20, hinstance, f_body);
    y += 32;

    // 外观
    y = section(hwnd, y, sec_idx, "外观", hinstance, f_body);
    sec_idx += 1;
    combo_row(hwnd, y, "皮肤", ID_SKIN, hinstance, f_body);
    y += ROW_H;
    numeric_row(hwnd, y, "显示尺寸 (px)", ID_DISPLAY, hinstance, f_body);
    y += ROW_H;
    numeric_row(hwnd, y, "每帧时长 (ms)", ID_FRAME, hinstance, f_body);
    y += ROW_H + 10;

    // 模式
    y = section(hwnd, y, sec_idx, "显示模式", hinstance, f_body);
    sec_idx += 1;
    mk(hwnd, w!("BUTTON"), "挂件模式", WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTORADIOBUTTON as u32) | WS_GROUP, PAD, y + 3, 110, 22, ID_MODE_PENDANT, hinstance, f_body);
    mk(hwnd, w!("BUTTON"), "停靠模式", WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTORADIOBUTTON as u32), PAD + 120, y + 3, 110, 22, ID_MODE_DOCK, hinstance, f_body);
    y += ROW_H + 10;

    // 挂件模式
    y = section(hwnd, y, sec_idx, "挂件模式", hinstance, f_body);
    sec_idx += 1;
    numeric_row(hwnd, y, "绳子长度 (px)", ID_ROPE, hinstance, f_body);
    y += ROW_H;
    numeric_row(hwnd, y, "重力 (px/s²)", ID_GRAVITY, hinstance, f_body);
    y += ROW_H;
    numeric_row(hwnd, y, "阻尼", ID_DAMPING, hinstance, f_body);
    y += ROW_H;
    numeric_row(hwnd, y, "最大摆角 (°)", ID_ANGLE, hinstance, f_body);
    y += ROW_H;
    numeric_row(hwnd, y, "光标间隙 (px)", ID_CLEARANCE, hinstance, f_body);
    y += ROW_H + 10;

    // 停靠模式
    y = section(hwnd, y, sec_idx, "停靠模式", hinstance, f_body);
    sec_idx += 1;
    numeric_row(hwnd, y, "横向偏移 X", ID_DOCK_X, hinstance, f_body);
    y += ROW_H;
    numeric_row(hwnd, y, "纵向偏移 Y", ID_DOCK_Y, hinstance, f_body);
    y += ROW_H + 10;

    // 刷新与性能
    y = section(hwnd, y, sec_idx, "刷新与性能", hinstance, f_body);
    numeric_row(hwnd, y, "光标采样 (ms)", ID_POLL, hinstance, f_body);
    y += ROW_H;
    numeric_row(hwnd, y, "物理频率 (Hz)", ID_PHYS, hinstance, f_body);
    y += ROW_H + 18;

    // 按钮
    let quit_x = CLIENT_W - PAD - 96;
    let apply_x = quit_x - 12 - 96;
    mk(hwnd, w!("BUTTON"), "应用", WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32), apply_x, y, 96, 36, ID_APPLY, hinstance, f_body);
    mk(hwnd, w!("BUTTON"), "退出", WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32), quit_x, y, 96, 36, ID_QUIT, hinstance, f_body);

    populate(hwnd);
    y + 36 + 22
}

unsafe fn section(
    hwnd: HWND,
    y: i32,
    idx: i32,
    text: &str,
    hinstance: HINSTANCE,
    font: HFONT,
) -> i32 {
    label(hwnd, ID_SEC_BASE + idx, text, PAD, y, 300, 20, hinstance, font);
    SECTIONS.lock().unwrap().push(y + 2);
    y + 24
}

unsafe fn combo_row(
    hwnd: HWND,
    y: i32,
    text: &str,
    id: i32,
    hinstance: HINSTANCE,
    font: HFONT,
) {
    label(hwnd, 0, text, PAD, y + 4, LABEL_W, 20, hinstance, font);
    mk(
        hwnd,
        w!("COMBOBOX"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
        FIELD_X,
        y - 1,
        FIELD_W,
        200,
        id,
        hinstance,
        font,
    );
}

/// 一行数值设置：标签 + 输入框 + 上下按钮
unsafe fn numeric_row(
    hwnd: HWND,
    y: i32,
    text: &str,
    id: i32,
    hinstance: HINSTANCE,
    font: HFONT,
) {
    label(hwnd, 0, text, PAD, y + 4, LABEL_W, 20, hinstance, font);
    let edit = mk(
        hwnd,
        w!("EDIT"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32) | WS_BORDER,
        FIELD_X,
        y,
        FIELD_W,
        FIELD_H,
        id,
        hinstance,
        font,
    );
    // 原生上下按钮，交给控件自己把整数写回 buddy
    let up = mk(
        hwnd,
        w!("msctls_updown32"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WINDOW_STYLE(UDS_ALIGNRIGHT as u32)
            | WINDOW_STYLE(UDS_ARROWKEYS as u32)
            | WINDOW_STYLE(UDS_SETBUDDYINT as u32),
        FIELD_X,
        y,
        0,
        FIELD_H,
        id + UP_OFFSET,
        hinstance,
        font,
    );
    SendMessageW(
        up,
        UDM_SETBUDDY,
        Some(WPARAM(edit.0 as usize)),
        Some(LPARAM(0)),
    );
    let (lo, hi) = range_of(id);
    SendMessageW(
        up,
        UDM_SETRANGE32,
        Some(WPARAM(lo as usize)),
        Some(LPARAM(hi as isize)),
    );
}

/// 静态文本。id 用于在 WM_CTLCOLORSTATIC 里区分标题/分组/普通标签。
#[allow(clippy::too_many_arguments)]
unsafe fn label(
    parent: HWND,
    id: i32,
    text: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    hinstance: HINSTANCE,
    font: HFONT,
) -> HWND {
    let t = wide(text);
    let ctrl = CreateWindowExW(
        WS_EX_TRANSPARENT,
        w!("STATIC"),
        PCWSTR(t.as_ptr()),
        WS_CHILD | WS_VISIBLE,
        x,
        y,
        w,
        h,
        Some(parent),
        Some(HMENU(id as isize as *mut _)),
        Some(hinstance),
        None,
    )
    .unwrap_or_default();
    SendMessageW(ctrl, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    ctrl
}

#[allow(clippy::too_many_arguments)]
unsafe fn mk(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: i32,
    hinstance: HINSTANCE,
    font: HFONT,
) -> HWND {
    let t = wide(text);
    let ctrl = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        PCWSTR(t.as_ptr()),
        style,
        x,
        y,
        w,
        h,
        Some(parent),
        Some(HMENU(id as isize as *mut _)),
        Some(hinstance),
        None,
    )
    .unwrap_or_default();
    SendMessageW(ctrl, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    ctrl
}

fn ctrl(hwnd: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(Some(hwnd), id).unwrap_or_default() }
}

unsafe fn text_of(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = GetWindowTextW(h, &mut buf);
    if n <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

unsafe fn set_text(h: HWND, s: &str) {
    let w = wide(s);
    let _ = SetWindowTextW(h, PCWSTR(w.as_ptr()));
}

fn populate(hwnd: HWND) {
    let cfg = CFG.lock().unwrap().clone().unwrap_or_default();
    let exe_dir = EXE_DIR.lock().unwrap().clone().unwrap_or_default();

    unsafe {
        let combo = ctrl(hwnd, ID_SKIN);
        SendMessageW(combo, CB_RESETCONTENT, Some(WPARAM(0)), Some(LPARAM(0)));
        let skins = list_skins(&exe_dir);
        let mut sel = 0usize;
        for (i, name) in skins.iter().enumerate() {
            let w = wide(name);
            SendMessageW(
                combo,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(w.as_ptr() as isize)),
            );
            if *name == cfg.skin {
                sel = i;
            }
        }
        if !skins.is_empty() {
            SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(sel)), Some(LPARAM(0)));
        }

        sync_updown(hwnd, ID_DISPLAY, cfg.display_size as i32);
        sync_updown(hwnd, ID_FRAME, cfg.frame_ms as i32);
        sync_updown(hwnd, ID_ROPE, cfg.rope_length.round() as i32);
        sync_updown(hwnd, ID_GRAVITY, cfg.gravity.round() as i32);
        sync_updown(hwnd, ID_DAMPING, cfg.damping.round() as i32);
        sync_updown(hwnd, ID_ANGLE, cfg.max_angle_deg.round() as i32);
        sync_updown(hwnd, ID_CLEARANCE, cfg.cursor_clearance);
        sync_updown(hwnd, ID_DOCK_X, cfg.dock_x);
        sync_updown(hwnd, ID_DOCK_Y, cfg.dock_y);
        sync_updown(hwnd, ID_POLL, cfg.poll_ms as i32);
        sync_updown(hwnd, ID_PHYS, cfg.physics_hz as i32);

        set_text(ctrl(hwnd, ID_DISPLAY), &cfg.display_size.to_string());
        set_text(ctrl(hwnd, ID_FRAME), &cfg.frame_ms.to_string());
        set_text(ctrl(hwnd, ID_ROPE), &fmt(cfg.rope_length));
        set_text(ctrl(hwnd, ID_GRAVITY), &fmt(cfg.gravity));
        set_text(ctrl(hwnd, ID_DAMPING), &fmt(cfg.damping));
        set_text(ctrl(hwnd, ID_ANGLE), &fmt(cfg.max_angle_deg));
        set_text(ctrl(hwnd, ID_CLEARANCE), &cfg.cursor_clearance.to_string());
        set_text(ctrl(hwnd, ID_DOCK_X), &cfg.dock_x.to_string());
        set_text(ctrl(hwnd, ID_DOCK_Y), &cfg.dock_y.to_string());
        set_text(ctrl(hwnd, ID_POLL), &cfg.poll_ms.to_string());
        set_text(ctrl(hwnd, ID_PHYS), &cfg.physics_hz.to_string());

        let checked = match cfg.mode {
            Mode::Dock => ID_MODE_DOCK,
            Mode::Pendant => ID_MODE_PENDANT,
        };
        let _ = CheckRadioButton(hwnd, ID_MODE_PENDANT, ID_MODE_DOCK, checked);
    }
}

fn fmt(v: f32) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn read_config(hwnd: HWND) -> Config {
    let mut cfg = CFG.lock().unwrap().clone().unwrap_or_default();
    unsafe {
        let combo = ctrl(hwnd, ID_SKIN);
        let idx = SendMessageW(combo, CB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0;
        if idx >= 0 {
            let mut buf = [0u16; 260];
            let n = SendMessageW(
                combo,
                CB_GETLBTEXT,
                Some(WPARAM(idx as usize)),
                Some(LPARAM(buf.as_mut_ptr() as isize)),
            )
            .0;
            if n > 0 {
                cfg.skin = String::from_utf16_lossy(&buf[..n as usize]);
            }
        }

        let f = |id: i32, def: f32| -> f32 {
            text_of(ctrl(hwnd, id))
                .trim()
                .parse::<f32>()
                .unwrap_or(def)
        };
        let u = |id: i32, def: u32| -> u32 {
            text_of(ctrl(hwnd, id))
                .trim()
                .parse::<u32>()
                .unwrap_or(def)
        };
        let i = |id: i32, def: i32| -> i32 {
            text_of(ctrl(hwnd, id))
                .trim()
                .parse::<i32>()
                .unwrap_or(def)
        };

        cfg.mode = if IsDlgButtonChecked(hwnd, ID_MODE_DOCK) == BST_CHECKED.0 {
            Mode::Dock
        } else {
            Mode::Pendant
        };

        cfg.display_size = u(ID_DISPLAY, cfg.display_size).clamp(16, 512);
        cfg.frame_ms = u(ID_FRAME, cfg.frame_ms).clamp(10, 1000);
        cfg.rope_length = f(ID_ROPE, cfg.rope_length).clamp(0.0, 400.0);
        cfg.gravity = f(ID_GRAVITY, cfg.gravity).clamp(100.0, 20000.0);
        cfg.damping = f(ID_DAMPING, cfg.damping).clamp(0.0, 30.0);
        cfg.max_angle_deg = f(ID_ANGLE, cfg.max_angle_deg).clamp(1.0, 80.0);
        cfg.cursor_clearance = i(ID_CLEARANCE, cfg.cursor_clearance).clamp(0, 200);
        cfg.dock_x = i(ID_DOCK_X, cfg.dock_x).clamp(-2000, 2000);
        cfg.dock_y = i(ID_DOCK_Y, cfg.dock_y).clamp(-2000, 2000);
        cfg.poll_ms = u(ID_POLL, cfg.poll_ms).clamp(1, 50);
        cfg.physics_hz = u(ID_PHYS, cfg.physics_hz).clamp(30, 1000);
    }
    cfg
}

/// 数值框的取值范围（交给原生上下按钮钳制）
fn range_of(field: i32) -> (i32, i32) {
    match field {
        ID_DISPLAY => (16, 512),
        ID_FRAME => (10, 1000),
        ID_ROPE => (0, 400),
        ID_GRAVITY => (100, 20000),
        ID_DAMPING => (0, 30),
        ID_ANGLE => (1, 80),
        ID_CLEARANCE => (0, 200),
        ID_DOCK_X | ID_DOCK_Y => (-2000, 2000),
        ID_POLL => (1, 50),
        ID_PHYS => (30, 1000),
        _ => (-1000000, 1000000),
    }
}

/// 把当前值同步给上下按钮的内部位置。
/// 必须在 set_text 之前调用：开了 SETBUDDYINT 后控件会把位置写回文本，
/// 先同步位置再写文本，打开界面时显示的仍是配置文件里的原始值。
unsafe fn sync_updown(hwnd: HWND, field: i32, value: i32) {
    let up = ctrl(hwnd, field + UP_OFFSET);
    SendMessageW(up, UDM_SETPOS32, Some(WPARAM(0)), Some(LPARAM(value as isize)));
}

unsafe fn draw_button(dis: &DRAWITEMSTRUCT) {
    let pressed = (dis.itemState.0 & ODS_SELECTED.0) != 0;
    let primary = dis.CtlID == ID_APPLY as u32;

    let (fill, border, text) = if primary {
        (
            if pressed { C_ACCENT_DOWN } else { C_ACCENT },
            if pressed { C_ACCENT_DOWN } else { C_ACCENT },
            C_WHITE,
        )
    } else {
        (
            if pressed { C_OUTLINE_DOWN } else { C_WHITE },
            if pressed { C_ACCENT } else { C_OUTLINE },
            C_TITLE,
        )
    };

    let brush = CreateSolidBrush(fill);
    let pen = CreatePen(PS_SOLID, 1, border);
    let ob = SelectObject(dis.hDC, HGDIOBJ(brush.0));
    let op = SelectObject(dis.hDC, HGDIOBJ(pen.0));
    let _ = RoundRect(
        dis.hDC,
        dis.rcItem.left,
        dis.rcItem.top,
        dis.rcItem.right,
        dis.rcItem.bottom,
        10,
        10,
    );
    SelectObject(dis.hDC, ob);
    SelectObject(dis.hDC, op);
    let _ = DeleteObject(HGDIOBJ(brush.0));
    let _ = DeleteObject(HGDIOBJ(pen.0));

    let font = HFONT(F_BODY.load(Ordering::SeqCst) as *mut _);
    let of = SelectObject(dis.hDC, HGDIOBJ(font.0));
    SetBkMode(dis.hDC, TRANSPARENT);
    SetTextColor(dis.hDC, text);

    let label = if primary { "应用" } else { "退出" };
    let mut w = wide(label);
    let n = w.len() - 1;
    let mut rc = dis.rcItem;
    DrawTextW(
        dis.hDC,
        &mut w[..n],
        &mut rc,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
    SelectObject(dis.hDC, of);
}

unsafe extern "system" fn ui_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            if let Some(ico) = crate::icon::app_icon() {
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(ico.0 as isize)));
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), Some(LPARAM(ico.0 as isize)));
            }
            let h = create_children(hwnd);
            fit_to_content(hwnd, h);
            LRESULT(0)
        }
        // 分组分隔线
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let pen = CreatePen(PS_SOLID, 1, C_LINE);
            let op = SelectObject(hdc, HGDIOBJ(pen.0));
            for y in SECTIONS.lock().unwrap().iter() {
                let _ = MoveToEx(hdc, PAD, y - 9, None);
                let _ = LineTo(hdc, CLIENT_W - PAD, y - 9);
            }
            SelectObject(hdc, op);
            let _ = DeleteObject(HGDIOBJ(pen.0));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut _);
            let id = GetDlgCtrlID(HWND(lparam.0 as *mut _));
            let color = match id {
                ID_TITLE => C_TITLE,
                ID_SUB => C_MUTED,
                i if (ID_SEC_BASE..ID_SEC_BASE + 50).contains(&i) => C_SECTION,
                _ => C_TITLE,
            };
            SetTextColor(hdc, color);
            SetBkColor(hdc, C_BG);
            LRESULT(BRUSH_BG.load(Ordering::SeqCst))
        }
        WM_DRAWITEM => {
            let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if dis.CtlType == ODT_BUTTON {
                draw_button(dis);
                return LRESULT(1);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            match id {
                ID_APPLY => {
                    let cfg = read_config(hwnd);
                    *CFG.lock().unwrap() = Some(cfg.clone());
                    CMDS.lock().unwrap().push(Cmd::Apply(cfg));
                    LRESULT(0)
                }
                ID_QUIT => {
                    CMDS.lock().unwrap().push(Cmd::Exit);
                    LRESULT(0)
                }
                _ => LRESULT(0),
            }
        }
        WM_CLOSE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_DESTROY => {
            HWND_UI.store(0, Ordering::SeqCst);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
