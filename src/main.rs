#![windows_subsystem = "windows"]

mod config;
mod cursor;
mod icon;
mod physics;
mod render;
mod settings;
mod sprite;
mod tray;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Instant;

use windows::core::{w, Result, HSTRING};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::Sleep;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL,
};
use windows::Win32::UI::Controls::{InitCommonControlsEx, ICC_STANDARD_CLASSES, ICC_UPDOWN_CLASS, INITCOMMONCONTROLSEX};
use windows::Win32::UI::WindowsAndMessaging::*;

use config::{Config, Mode};
use cursor::{CursorState, CursorWatcher};
use physics::{PivotTracker, Rope};
use render::Renderer;
use sprite::FrameSet;

const HOTKEY_ID: i32 = 1;
/// 需要重建合成链路（遮挡恢复 / 绘制失败时置位）
static REATTACH: AtomicBool = AtomicBool::new(false);
/// 正在重建窗口 —— 此时 WM_DESTROY 不能退出程序
static RECREATING: AtomicBool = AtomicBool::new(false);
/// 收到会话解锁通知，等主循环处理（不能在 wndproc 里销毁自己的窗口）
static UNLOCKED: AtomicBool = AtomicBool::new(false);
/// 进程启动时刻，用于解锁重启的防抖
static RUN_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// 会话锁屏/解锁消息（wtsapi32.h）
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
/// 0x8 = 解锁（0x7 是锁屏，这里只需要解锁）
const WTS_SESSION_UNLOCK: usize = 0x8;

static QUIT: AtomicBool = AtomicBool::new(false);

fn main() {
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_UPDOWN_CLASS,
        };
        let _ = InitCommonControlsEx(&icc);
    }
    if let Err(e) = run() {
        unsafe {
            let msg = HSTRING::from(format!("{e:?}"));
            MessageBoxW(None, &msg, w!("RustWidget 启动失败"), MB_ICONERROR | MB_OK);
        }
    }
}

/// 一套皮肤在运行期的加载结果。
struct SkinRuntime {
    resolved: Vec<&'static str>,
    /// 磁盘上不存在的状态目录 → 回落到「正常选择」
    alias: HashMap<&'static str, &'static str>,
    rx: Receiver<(&'static str, Arc<FrameSet>)>,
}

impl SkinRuntime {
    fn load(cfg: &Config, exe_dir: &Path) -> Self {
        let base = cfg.skin_dir(exe_dir);
        let normal_dir = CursorState::Normal.dir();

        let mut resolved: Vec<&'static str> = vec![normal_dir];
        let mut alias: HashMap<&'static str, &'static str> = HashMap::new();

        for key in CursorState::unique_dirs() {
            if key == normal_dir {
                continue;
            }
            if base.join(key).is_dir() {
                resolved.push(key);
            } else {
                alias.insert(key, normal_dir);
            }
        }

        let mut rest = resolved.clone();
        rest.retain(|k| *k != normal_dir);
        let rx = sprite::spawn_loader(base.clone(), cfg.display_size, rest);

        Self {
            resolved,
            alias,
            rx,
        }
    }

    fn key_for(&self, state: CursorState) -> &'static str {
        let want = state.dir();
        if self.resolved.contains(&want) {
            want
        } else {
            *self.alias.get(want).unwrap_or(&CursorState::Normal.dir())
        }
    }
}

fn run() -> Result<()> {
    let exe_dir = exe_dir();
    let mut cfg = Config::load(&exe_dir.join("widget.toml"));

    // 先把 Normal 同步加载好，拿到真实纹理尺寸后才能算窗口大小
    let mut normal = load_normal(&cfg, &exe_dir);
    let (mut sprite_w, mut sprite_h) = (normal.width, normal.height);
    let (mut win_w, mut win_h) = cfg.window_size_for(sprite_w, sprite_h);

    let mut skin = SkinRuntime::load(&cfg, &exe_dir);

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let mut hwnd = create_window(win_w, win_h)?;
        let mut renderer = Renderer::new(hwnd, win_w, win_h)?;
        renderer.upload_state(CursorState::Normal.dir(), &normal)?;
        renderer.set_active(CursorState::Normal.dir());
        renderer.upload_rope(&build_rope(&cfg, sprite_h))?;
        renderer.set_offset(0, 0)?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        register_session_notify(hwnd);
        let _ = RUN_START.set(Instant::now());

        let tray_ok = tray::add(hwnd).is_ok();
        let _ = RegisterHotKey(Some(hwnd), HOTKEY_ID, MOD_CONTROL | MOD_ALT, 'Q' as u32);

        settings::sync(&cfg, &exe_dir);

        let mut watcher = CursorWatcher::new();
        let mut tracker = PivotTracker::new();
        let mut rope = Rope::new(
            cfg.rope_segments as usize,
            rope_total(&cfg, sprite_h),
            [0.0, 0.0],
        );
        rope.gravity = cfg.gravity;
        rope.damping = cfg.damping * 0.5;
        rope.max_theta = cfg.max_angle_deg.to_radians();
        let mut sim_acc = 0.0f32;

        let mut half_w = win_w as f32 * 0.5;
        let mut frame_dur = cfg.frame_ms as f32 / 1000.0;
        let mut fixed_dt = 1.0 / cfg.physics_hz as f32;

        let mut shown_dir = CursorState::Normal.dir();
        let mut frame_idx: usize = 0;
        let mut frame_acc: f32 = 0.0;
        let mut last = Instant::now();
        let mut last_retry = Instant::now();
        let mut last_cursor = POINT::default();
        // 上次真正下发的窗口位置。位置没变就不再调 SetWindowPos，
        // 否则静止时也会以循环频率反复"移动到同一位置"，纯属浪费系统调用。
        let mut last_win_pos = (i32::MIN, i32::MIN);

        loop {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if QUIT.load(Ordering::SeqCst) {
                break;
            }

            // 遮挡恢复（显示器休眠唤醒、锁屏解锁、全屏切换）/ 上次绘制失败 → 重建合成链路
            // 会话解锁 → 重启进程恢复画面
            if UNLOCKED.swap(false, Ordering::SeqCst) {
                let alive = RUN_START.get().map_or(0, |t| t.elapsed().as_secs());
                if alive >= 3 {
                    relaunch_and_exit();
                }
            }
            // 重建失败后隔 2 秒再重试，避免每帧刷日志
            if REATTACH.load(Ordering::SeqCst) && last_retry.elapsed().as_millis() >= 2000 {
                REATTACH.store(false, Ordering::SeqCst);
                last_retry = Instant::now();
                match rebuild_window(&mut hwnd, &mut renderer, win_w, win_h, tray_ok) {
                    Ok(()) => last_win_pos = (i32::MIN, i32::MIN),
                    Err(_) => REATTACH.store(true, Ordering::SeqCst),
                }
            }

            for cmd in settings::take_cmds() {
                match cmd {
                    settings::Cmd::Exit => QUIT.store(true, Ordering::SeqCst),
                    settings::Cmd::Apply(new_cfg) => {
                        let need_reload = new_cfg.skin != cfg.skin
                            || new_cfg.display_size != cfg.display_size;
                        cfg = new_cfg;

                        if need_reload {
                            renderer.clear_states();
                            skin = SkinRuntime::load(&cfg, &exe_dir);
                            normal = load_normal(&cfg, &exe_dir);
                            sprite_w = normal.width;
                            sprite_h = normal.height;
                            renderer.upload_state(CursorState::Normal.dir(), &normal)?;
                            renderer.set_active(CursorState::Normal.dir());
                            shown_dir = CursorState::Normal.dir();
                            frame_idx = 0;
                            frame_acc = 0.0;
                        }

                        let (nw, nh) = cfg.window_size_for(sprite_w, sprite_h);
                        if (nw, nh) != (win_w, win_h) {
                            win_w = nw;
                            win_h = nh;
                            let _ = SetWindowPos(
                                hwnd,
                                None,
                                0,
                                0,
                                win_w as i32,
                                win_h as i32,
                                SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                            );
                            renderer.resize(win_w, win_h)?;
                            half_w = win_w as f32 * 0.5;
                        }

                        renderer.upload_rope(&build_rope(&cfg, sprite_h))?;

                        frame_dur = cfg.frame_ms as f32 / 1000.0;
                        fixed_dt = 1.0 / cfg.physics_hz as f32;
                        // 段数变了必须重建绳链：段长随段数变化，不能只改字段
                        if rope.segments() != cfg.rope_segments as usize {
                            rope = Rope::new(
                                cfg.rope_segments as usize,
                                rope_total(&cfg, sprite_h),
                                [half_w, 0.0],
                            );
                        } else {
                            rope.set_total_len(rope_total(&cfg, sprite_h));
                        }
                        rope.gravity = cfg.gravity;
                        rope.damping = cfg.damping * 0.5;
                        rope.max_theta = cfg.max_angle_deg.to_radians();

                        settings::sync(&cfg, &exe_dir);
                        let _ = cfg.save(&exe_dir.join("widget.toml"));
                    }
                }
            }

            while let Ok((key, fs)) = skin.rx.try_recv() {
                renderer.upload_state(key, &fs)?;
            }

            let now = Instant::now();
            let dt = (now - last).as_secs_f32();
            last = now;

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);

            // 时间跳变说明进程被挂起过（系统休眠，或 Present 长时间阻塞）。
            // 这种情况下合成链路一定失效，而且期间没有 Present 返回值可看，遮挡检测抓不到。
            if dt > 1.5 {
                tracker.reset([pt.x as f32, pt.y as f32]);
                REATTACH.store(true, Ordering::SeqCst);
            }

            let accel = tracker.update([pt.x as f32, pt.y as f32], dt);
            if cfg.mode == Mode::Pendant {
                // 固定步长积分，物理行为与帧率解耦
                sim_acc = (sim_acc + dt).min(0.1);
                let pivot = [half_w, 0.0];
                let mut steps = 0;
                while sim_acc >= fixed_dt && steps < 24 {
                    rope.step(fixed_dt, pivot, accel);
                    sim_acc -= fixed_dt;
                    steps += 1;
                }
            }

            // 挂件模式：窗口下移 cursor_clearance，让光标热点落在窗口之外
            // 停靠模式：窗口正好等于精灵大小，左上角放在光标 + 偏移
            let (wx, wy) = match cfg.mode {
                Mode::Dock => (pt.x + cfg.dock_x, pt.y + cfg.dock_y),
                Mode::Pendant => (pt.x - (win_w / 2) as i32, pt.y + cfg.cursor_clearance),
            };
            if (wx, wy) != last_win_pos {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    wx,
                    wy,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
                last_win_pos = (wx, wy);
            }

            let state = watcher.poll(now);
            let want = skin.key_for(state);
            let mut state_changed = false;
            if want != shown_dir && renderer.set_active(want) {
                shown_dir = want;
                frame_idx = 0;
                frame_acc = 0.0;
                state_changed = true;
            }

            let moved = pt.x != last_cursor.x || pt.y != last_cursor.y;
            last_cursor = pt;

            frame_acc += dt;
            let mut frame_changed = false;
            if frame_acc >= frame_dur {
                let adv = (frame_acc / frame_dur) as usize;
                frame_idx += adv;
                frame_acc -= adv as f32 * frame_dur;
                frame_changed = true;
            }

            let swinging = cfg.mode == Mode::Pendant && !rope.is_at_rest();
            let dirty = frame_changed || moved || state_changed || swinging;
            if dirty {
                let drawn = (|| -> Result<()> {
                renderer.begin_frame()?;
                match cfg.mode {
                    Mode::Dock => {
                        // 停靠：精灵居中、不旋转、不画绳子
                        let center = (win_w as f32 * 0.5, win_h as f32 * 0.5);
                        renderer.draw_sprite(center, 0.0, frame_idx as u32)?;
                    }
                    Mode::Pendant => {
                        let (_, sh) = renderer.active_size();
                        let hh = sh as f32 * 0.5;
                        let theta = rope.tip_angle();
                        let tip = rope.tip();
                        let dir = (theta.sin(), theta.cos());

                        // 精灵顶边贴在绳端，再沿绳子方向把中心推进半个高度，
                        // 让绳子末端有半个精灵高度伸进画布、被角色挡住
                        let center = (tip[0] + hh * 0.5 * dir.0, tip[1] + hh * 0.5 * dir.1);
                        let rope_w = (cfg.display_size / 12).clamp(2, 6) as f32;

                        renderer.draw_rope(&rope.pts, rope_w)?;
                        renderer.draw_sprite(center, theta, frame_idx as u32)?;
                    }
                }
                renderer.end_frame()?;
                Ok(())
                })();
                if drawn.is_err() {
                    // 单帧失败不该结束进程：多半是设备/合成链路失效，重建一次
                    REATTACH.store(true, Ordering::SeqCst);
                    Sleep(16);
                }
            } else {
                // 空闲时休眠，决定鼠标位置的采样间隔
                Sleep(cfg.poll_ms);
            }
        }

        if tray_ok {
            tray::remove(hwnd);
        }
        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
        let _ = DestroyWindow(hwnd);
    }

    Ok(())
}

/// 加载「正常选择」这一套。任何环节失败都退化成全黑方块，保证挂件不会因为素材问题消失。
fn load_normal(cfg: &Config, exe_dir: &Path) -> FrameSet {
    let dir = cfg.skin_dir(exe_dir).join(CursorState::Normal.dir());
    sprite::load_dir(&dir, cfg.display_size)
        .unwrap_or_else(|_| sprite::placeholder(cfg.display_size))
}

/// 绳链总长：配置的绳长 + 四分之一个精灵高度（让末端伸进画布）。
fn rope_total(cfg: &Config, sprite_h: u32) -> f32 {
    (cfg.rope_length + sprite_h as f32 * 0.25).max(8.0)
}

/// 绳子纹理：宽度随显示尺寸，长度按绳链总长生成，绘制时 1:1。
fn build_rope(cfg: &Config, sprite_h: u32) -> FrameSet {
    let hh = sprite_h as f32 * 0.5;
    let len = (cfg.rope_length + hh * 0.5).max(4.0).round() as u32;
    let w = (cfg.display_size / 12).clamp(2, 6);
    sprite::make_rope(w, len)
}

/// 连窗口一起重建。
///
/// 只重建 DirectComposition 对象不够：`WS_EX_NOREDIRECTIONBITMAP` 窗口的合成关联一旦
/// 在锁屏时被 DWM 丢弃，重建交换链和视觉树也挂不回去（实测窗口可见、无 cloaked、
/// 合成正常，但内容永远空白）。销毁并重新创建窗口才能让 DWM 重新建立关联。
unsafe fn rebuild_window(
    hwnd: &mut HWND,
    renderer: &mut Renderer,
    w: u32,
    h: u32,
    tray_ok: bool,
) -> Result<()> {
    let old = *hwnd;

    RECREATING.store(true, Ordering::SeqCst);
    let _ = UnregisterHotKey(Some(old), HOTKEY_ID);
    if tray_ok {
        tray::remove(old);
    }
    let _ = DestroyWindow(old);

    let new = create_window(w, h)?;
    *hwnd = new;

    let _ = ShowWindow(new, SW_SHOWNOACTIVATE);
    register_session_notify(new);
    if tray_ok {
        let _ = tray::add(new);
    }
    let _ = RegisterHotKey(Some(new), HOTKEY_ID, MOD_CONTROL | MOD_ALT, 'Q' as u32);

    renderer.reattach(new)?;
    let _ = renderer.set_offset(0, 0);

    RECREATING.store(false, Ordering::SeqCst);
    Ok(())
}

/// 订阅会话状态通知。锁屏/解锁时系统会往这个窗口发 WM_WTSSESSION_CHANGE。
/// 窗口重建后必须重新注册（通知绑定在 HWND 上）。
fn register_session_notify(hwnd: HWND) {
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    unsafe {
        let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
    }
}

/// 重启自身。锁屏会污染进程内的 GPU/合成状态，
/// 实测连「销毁并重建窗口 + 重建交换链」都救不回来，只有新进程能恢复。
fn relaunch_and_exit() {
    use std::os::windows::process::CommandExt;
    /// 让子进程脱离父进程所在的 Job 对象，否则父进程退出时可能被一起清理
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(&exe);
        cmd.creation_flags(CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_err() {
            // 脱离 Job 被拒绝时退回普通方式
            let _ = std::process::Command::new(&exe).spawn();
        }
    }
    QUIT.store(true, Ordering::SeqCst);
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn create_window(width: u32, height: u32) -> Result<HWND> {
    unsafe {
        let hmodule = GetModuleHandleW(None)?;
        let hinstance = HINSTANCE(hmodule.0);

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("RustWidgetWnd"),
            ..Default::default()
        };
        // 类只需注册一次。重建窗口时再注册会返回 ERROR_CLASS_ALREADY_EXISTS（1410），
        // 那说明类已经就绪，不是失败；真正的失败交给下面的 CreateWindowExW 报。
        let _ = RegisterClassW(&wc);

        let ex_style = WS_EX_NOREDIRECTIONBITMAP
            | WS_EX_TRANSPARENT
            | WS_EX_TOPMOST
            | WS_EX_TOOLWINDOW
            | WS_EX_NOACTIVATE;

        CreateWindowExW(
            ex_style,
            w!("RustWidgetWnd"),
            w!("RustWidget"),
            WS_POPUP,
            0,
            0,
            width as i32,
            height as i32,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_HOTKEY => {
            QUIT.store(true, Ordering::SeqCst);
            LRESULT(0)
        }
        tray::WM_TRAY => {
            let event = (lparam.0 as u32) & 0xffff;
            if event == WM_LBUTTONUP || event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                settings::show_current();
            }
            LRESULT(0)
        }
        // 系统推送的会话状态变化
        WM_WTSSESSION_CHANGE => {
            if wparam.0 == WTS_SESSION_UNLOCK {
                UNLOCKED.store(true, Ordering::SeqCst);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if !RECREATING.load(Ordering::SeqCst) {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
