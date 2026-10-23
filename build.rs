use std::path::PathBuf;
use std::process::Command;

fn main() {
    // 1) 把 app.manifest 嵌进 exe：启用 comctl32 v6（现代控件外观）+ per-monitor v2 DPI
    let manifest = std::fs::canonicalize("app.manifest").expect("app.manifest 缺失");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );

    // 2) 编译图标资源
    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let res = out.join("icon.res");

    match find_rc() {
        Some(rc) => {
            let status = Command::new(&rc)
                .arg("/nologo")
                .arg("/I")
                .arg("assets")
                .arg("/fo")
                .arg(&res)
                .arg("assets\\icon.rc")
                .status();

            match status {
                Ok(s) if s.success() => {
                    println!("cargo:rustc-link-arg-bins={}", res.display());
                }
                _ => println!("cargo:warning=rc.exe 执行失败，图标未嵌入"),
            }
        }
        None => println!("cargo:warning=未找到 Windows SDK 的 rc.exe，图标未嵌入"),
    }
}

/// 在 Windows SDK 里找最新的 x64 rc.exe。
///
/// 不读注册表（reg.exe 可能被安全策略拦），直接扫常见安装路径。
fn find_rc() -> Option<PathBuf> {
    let roots: Vec<PathBuf> = vec![
        PathBuf::from(r"D://Windows Kits//10"),
        PathBuf::from(r"C://Program Files (x86)//Windows Kits//10"),
        PathBuf::from(r"C://Program Files//Windows Kits//10"),
    ];

    for root in roots {
        let bin = root.join("bin");
        let Ok(entries) = std::fs::read_dir(&bin) else {
            continue;
        };
        // 版本目录形如 10.0.26100.0，挑版本号最大的
        let mut versions: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        versions.sort();
        for v in versions.iter().rev() {
            let rc = v.join("x64").join("rc.exe");
            if rc.is_file() {
                return Some(rc);
            }
        }
    }
    None
}
