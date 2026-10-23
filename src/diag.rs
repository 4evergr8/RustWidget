use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static START: OnceLock<Instant> = OnceLock::new();

/// 初始化日志：写到 exe 同级的 rustwidget.log。
///
/// **追加而不是清空**——重启进程恢复画面时，如果每次启动都清空，
/// 上一个进程记录的「检测到解锁 → 重启」证据就被抹掉了。超过 256KB 才重置。
pub fn init(dir: &std::path::Path) {
    START.get_or_init(Instant::now);
    let p = dir.join("rustwidget.log");

    if let Ok(meta) = std::fs::metadata(&p) {
        if meta.len() > 256 * 1024 {
            let _ = std::fs::write(&p, b"");
        }
    }

    *LOG_PATH.lock().unwrap() = Some(p);
    log("=== RustWidget 启动 ===");
}

/// 记一行日志。同时写 stdout 和文件，每行立即 flush，崩溃也不丢。
pub fn log(msg: &str) {
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    let line = format!("[{t:>9.3}s] {msg}");
    println!("{line}");

    let guard = LOG_PATH.lock().unwrap();
    if let Some(p) = guard.as_ref() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

/// 带格式化的日志
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        $crate::diag::log(&format!($($arg)*))
    };
}
