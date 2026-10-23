use std::path::Path;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use image::imageops::FilterType;

/// 一组序列帧，全部为**预乘 alpha** 的 RGBA8，尺寸统一（可以是矩形）。
pub struct FrameSet {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<Vec<u8>>,
}

impl FrameSet {
    pub fn len(&self) -> usize {
        self.frames.len()
    }
}

/// 读取目录下所有 PNG，按文件名排序，缩放并转预乘 alpha。
///
/// `display_size` 是**最长边**的目标像素数：纹理按这个尺寸生成，绘制时 1:1 不做二次缩放，
/// 所以最终显示像素 = 纹理像素，最锐利。非正方形素材按比例归一化，不会被拉伸。
///
/// 顺序很重要：**先预乘、再缩放**。反过来的话 Lanczos 会把透明区域的 RGB
/// 混进边缘，抠图出来的头发/半透明边缘会出现暗边。
pub fn load_dir(dir: &Path, display_size: u32) -> Result<FrameSet, Box<dyn std::error::Error>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .map(|e| e.eq_ignore_ascii_case("png"))
                .unwrap_or(false)
        })
        .collect();

    if paths.is_empty() {
        return Err(format!("目录下没有 PNG：{}", dir.display()).into());
    }
    paths.sort();

    // 先只读文件头拿尺寸，取所有帧的最大外框，避免逐帧解码两遍
    let mut max_w = 0u32;
    let mut max_h = 0u32;
    for p in &paths {
        if let Ok((w, h)) = image::image_dimensions(p) {
            max_w = max_w.max(w);
            max_h = max_h.max(h);
        }
    }
    if max_w == 0 || max_h == 0 {
        return Err(format!("无法读取图片尺寸：{}", dir.display()).into());
    }

    // 最长边对齐到 display_size，另一边等比
    let scale = display_size as f32 / max_w.max(max_h) as f32;
    let tw = ((max_w as f32 * scale).round() as u32).max(1);
    let th = ((max_h as f32 * scale).round() as u32).max(1);

    let mut frames = Vec::with_capacity(paths.len());
    for p in &paths {
        let img = image::open(p)?.to_rgba8();
        let premul = premultiply(&img);
        let scaled = if premul.width() == tw && premul.height() == th {
            premul
        } else {
            image::imageops::resize(&premul, tw, th, FilterType::Lanczos3)
        };
        frames.push(scaled.into_raw());
    }

    Ok(FrameSet {
        width: tw,
        height: th,
        frames,
    })
}

/// 后台线程依次加载各状态，通过 channel 把结果送回主线程上传到 GPU。
/// GPU 资源必须在主线程创建，所以这里只做 CPU 侧的解码与缩放。
pub fn spawn_loader(
    root: std::path::PathBuf,
    display_size: u32,
    dirs: Vec<&'static str>,
) -> Receiver<(&'static str, Arc<FrameSet>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for key in dirs {
            let path = root.join(key);
            if let Ok(fs) = load_dir(&path, display_size) {
                if tx.send((key, Arc::new(fs))).is_err() {
                    return;
                }
            }
        }
    });
    rx
}

/// 占位图：全黑不透明正方形，单帧。
/// 当皮肤里找不到任何能加载的图片时用它顶上，保证挂件仍然可见而不是直接崩掉。
pub fn placeholder(size: u32) -> FrameSet {
    let n = size.max(1);
    let mut data = vec![0u8; (n * n * 4) as usize];
    for px in data.chunks_exact_mut(4) {
        px[3] = 255;
    }
    FrameSet {
        width: n,
        height: n,
        frames: vec![data],
    }
}

/// 程序化生成绳子纹理：横向圆柱明暗 + 斜向扭转，左右边缘抗锯齿。
/// 不依赖任何外部素材，长度随绳子实际长度变化，绘制时 1:1。
pub fn make_rope(width: u32, height: u32) -> FrameSet {
    let w = width.max(2);
    let h = height.max(4);
    let mut data = vec![0u8; (w * h * 4) as usize];

    for y in 0..h {
        for x in 0..w {
            let t = (x as f32 + 0.5) / w as f32;
            let d = (t - 0.5) * 2.0;
            // 圆柱明暗
            let shade = 1.0 - 0.30 * d * d;
            // 扭转纹：周期约 20px，对比很轻，避免像理发店转筒
            let twist = 0.90 + 0.10 * (((y as f32) * 0.30 + (x as f32) * 1.1).sin() * 0.5 + 0.5);
            let edge = (1.0 - (d.abs() - 0.55) / 0.45).clamp(0.0, 1.0);
            let a = (edge * 255.0).round() as u32;
            let base = 198.0 * shade * twist;

            let i = ((y * w + x) * 4) as usize;
            data[i] = (((base * 1.00) as u32 * a + 127) / 255) as u8;
            data[i + 1] = (((base * 0.94) as u32 * a + 127) / 255) as u8;
            data[i + 2] = (((base * 0.84) as u32 * a + 127) / 255) as u8;
            data[i + 3] = a as u8;
        }
    }

    FrameSet {
        width: w,
        height: h,
        frames: vec![data],
    }
}

fn premultiply(img: &image::RgbaImage) -> image::RgbaImage {
    let mut out = img.clone();
    for px in out.pixels_mut() {
        let a = px[3] as u32;
        if a == 255 {
            continue;
        }
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
        px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
        px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
    }
    out
}
