use std::path::{Path, PathBuf};

/// 显示模式
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// 挂件：绳子 + 单摆，从光标垂下
    Pendant,
    /// 停靠：精灵固定在光标旁边，不摆、不画绳子
    Dock,
}

/// 运行时配置。由设置界面写入 widget.toml，全部是扁平的 `key = value`。
#[derive(Clone)]
pub struct Config {
    pub mode: Mode,
    /// skin 目录下的子文件夹名，也是下拉菜单里显示的名字
    pub skin: String,
    /// 精灵**最长边**的显示像素。纹理直接按这个尺寸生成，显示时 1:1 不缩放。
    pub display_size: u32,
    pub frame_ms: u32,
    pub rope_length: f32,
    /// 绳子分几段。越多越柔软，物理开销按段数线性增长。
    pub rope_segments: u32,
    pub gravity: f32,
    pub damping: f32,
    pub max_angle_deg: f32,
    /// 窗口顶边相对光标热点下移的像素数。
    /// 必须 > 0，否则光标热点落在挂件窗口内，点击会被窗口吃掉。
    pub cursor_clearance: i32,
    /// 停靠模式下精灵左上角相对光标热点的偏移
    pub dock_x: i32,
    pub dock_y: i32,
    /// 空闲时主循环的休眠间隔（毫秒）。越小鼠标跟随越跟手，CPU 占用越高。
    pub poll_ms: u32,
    /// 物理积分频率（Hz）。固定步长 = 1/physics_hz。
    pub physics_hz: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Pendant,
            skin: "大肥鱼冬装".to_string(),
            display_size: 96,
            frame_ms: 100,
            rope_length: 60.0,
            rope_segments: 12,
            gravity: 2400.0,
            damping: 2.2,
            max_angle_deg: 35.0,
            cursor_clearance: 18,
            dock_x: 18,
            dock_y: 18,
            poll_ms: 8,
            physics_hz: 240,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Self {
        let mut cfg = Config::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            return cfg;
        };

        let mut map = std::collections::HashMap::new();
        for raw in text.lines() {
            let line = match raw.find('#') {
                Some(i) => &raw[..i],
                None => raw,
            }
            .trim();
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }

        let num = |key: &str, def: f64| -> f64 {
            map.get(key)
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(def)
        };

        if let Some(s) = map.get("skin") {
            let name = s.trim_matches('"').trim_matches('\'').to_string();
            if !name.is_empty() {
                cfg.skin = name;
            }
        }

        // 旧配置迁移：texture_size × sprite_scale = 显示边长
        if map.contains_key("display_size") {
            cfg.display_size = num("display_size", cfg.display_size as f64) as u32;
        } else if map.contains_key("texture_size") {
            let t = num("texture_size", 128.0);
            let s = num("sprite_scale", 0.9);
            cfg.display_size = (t * s).round().max(16.0) as u32;
        }

        cfg.frame_ms = num("frame_ms", cfg.frame_ms as f64) as u32;
        cfg.rope_length = num("rope_length", cfg.rope_length as f64) as f32;
        cfg.rope_segments = (num("rope_segments", cfg.rope_segments as f64) as u32).clamp(2, 30);
        cfg.gravity = num("gravity", cfg.gravity as f64) as f32;
        cfg.damping = num("damping", cfg.damping as f64) as f32;
        cfg.max_angle_deg = num("max_angle_deg", cfg.max_angle_deg as f64) as f32;
        cfg.cursor_clearance = num("cursor_clearance", cfg.cursor_clearance as f64) as i32;
        cfg.dock_x = num("dock_x", cfg.dock_x as f64) as i32;
        cfg.dock_y = num("dock_y", cfg.dock_y as f64) as i32;
        cfg.poll_ms = (num("poll_ms", cfg.poll_ms as f64) as u32).clamp(1, 50);
        cfg.physics_hz = (num("physics_hz", cfg.physics_hz as f64) as u32).clamp(30, 1000);

        if let Some(s) = map.get("mode") {
            let v = s.trim_matches('"').trim_matches('\'').to_ascii_lowercase();
            cfg.mode = if v == "dock" { Mode::Dock } else { Mode::Pendant };
        }

        cfg.display_size = cfg.display_size.clamp(16, 512);
        cfg
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = format!(
            "# RustWidget 配置（由设置界面写入）\n\
             # 素材根目录为 exe 同级的 skin/<皮肤名>/，切换皮肤只改 skin 一项\n\n\
             mode = \"{}\"\n\
             skin = \"{}\"\n\
             display_size = {}\n\
             frame_ms = {}\n\
             rope_length = {}\n\
             rope_segments = {}\n\
             gravity = {}\n\
             damping = {}\n\
             max_angle_deg = {}\n\
             cursor_clearance = {}\n\
             dock_x = {}\n\
             dock_y = {}\n\
             poll_ms = {}\n\
             physics_hz = {}\n",
            match self.mode {
                Mode::Pendant => "pendant",
                Mode::Dock => "dock",
            },
            self.skin,
            self.display_size,
            self.frame_ms,
            self.rope_length,
            self.rope_segments,
            self.gravity,
            self.damping,
            self.max_angle_deg,
            self.cursor_clearance,
            self.dock_x,
            self.dock_y,
            self.poll_ms,
            self.physics_hz
        );
        std::fs::write(path, text)
    }

    /// 按当前模式算窗口尺寸。停靠模式窗口正好等于精灵大小。
    pub fn window_size_for(&self, sprite_w: u32, sprite_h: u32) -> (u32, u32) {
        match self.mode {
            Mode::Dock => (sprite_w.max(1), sprite_h.max(1)),
            Mode::Pendant => self.window_size(sprite_w, sprite_h),
        }
    }

    /// 按最大摆幅算窗口尺寸。`sprite_w/h` 是纹理实际像素尺寸。
    ///
    /// 挂件绕自身中心旋转，所以不能只按半宽/半高算，要取旋转后的极值：
    ///   横向 = (L + hh)·sinθ + hw·cosθ
    ///   纵向 = (L + 2hh)·cosθ + hw·sinθ
    /// 两者都在 [0, θmax] 上求最大值（可能在区间内部取得）。
    pub fn window_size(&self, sprite_w: u32, sprite_h: u32) -> (u32, u32) {
        let hw = sprite_w as f32 * 0.5;
        let hh = sprite_h as f32 * 0.5;
        let tmax = self.max_angle_deg.to_radians();
        let l = self.rope_length;

        let half_x = extent(hw, l + hh, tmax);
        let bottom = extent(l + 2.0 * hh, hw, tmax);

        let w = (half_x * 2.0 + 12.0).ceil() as u32;
        let h = (bottom + 12.0).ceil() as u32;
        (w.max(48), h.max(48))
    }

    /// exe 同级的 skin 根目录
    pub fn skin_root(exe_dir: &Path) -> PathBuf {
        exe_dir.join("skin")
    }

    /// 当前皮肤目录
    pub fn skin_dir(&self, exe_dir: &Path) -> PathBuf {
        Self::skin_root(exe_dir).join(&self.skin)
    }
}

/// 求 `a·cosθ + b·sinθ` 在 θ ∈ [0, tmax] 上的最大值。
fn extent(a: f32, b: f32, tmax: f32) -> f32 {
    let mut m = a.max(a * tmax.cos() + b * tmax.sin());
    let peak = b.atan2(a);
    if peak > 0.0 && peak < tmax {
        m = m.max((a * a + b * b).sqrt());
    }
    m
}

/// 列出 skin 下的所有子文件夹名（即可选皮肤），按名称排序。
pub fn list_skins(exe_dir: &Path) -> Vec<String> {
    let root = Config::skin_root(exe_dir);
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}
