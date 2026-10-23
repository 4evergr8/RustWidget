pub type Pt = [f32; 2];

#[inline]
fn sub(a: Pt, b: Pt) -> Pt {
    [a[0] - b[0], a[1] - b[1]]
}

#[inline]
fn len(a: Pt) -> f32 {
    (a[0] * a[0] + a[1] * a[1]).sqrt()
}

/// 枢轴加速度估计。
///
/// 直接对鼠标坐标做二阶差分噪声极大（整数像素 + 系统 125Hz 采样），
/// 一阶指数平滑压噪的同时会把真实的竖直抖动一起抹掉。
/// 这里改成**临界阻尼弹簧跟踪器**：让 x 以弹簧方式跟随 target，
/// 跟踪时用到的加速度 `a` 本身就是一条平滑的加速度信号，直接拿来用。
pub struct PivotTracker {
    x: Pt,
    v: Pt,
    a: Pt,
    inited: bool,
}

/// 跟踪器自然角频率（rad/s）。40 ≈ 6.4Hz，覆盖人手抖绳子的频段。
const K: f32 = 1600.0;
/// 临界阻尼：c = 2√k
const C: f32 = 80.0;

impl PivotTracker {
    pub fn new() -> Self {
        Self {
            x: [0.0, 0.0],
            v: [0.0, 0.0],
            a: [0.0, 0.0],
            inited: false,
        }
    }

    pub fn reset(&mut self, p: Pt) {
        self.x = p;
        self.v = [0.0, 0.0];
        self.a = [0.0, 0.0];
        self.inited = true;
    }

    /// 喂入当前光标位置，返回平滑后的加速度（屏幕坐标，y 向下）。
    pub fn update(&mut self, target: Pt, dt: f32) -> Pt {
        if !self.inited {
            self.reset(target);
            return [0.0, 0.0];
        }
        let dt = dt.clamp(0.001, 0.05);

        let ax = K * (target[0] - self.x[0]) - C * self.v[0];
        let ay = K * (target[1] - self.x[1]) - C * self.v[1];

        self.v[0] += ax * dt;
        self.v[1] += ay * dt;
        self.x[0] += self.v[0] * dt;
        self.x[1] += self.v[1] * dt;

        // 限幅，避免瞬移（多屏切换）时把绳子甩飞
        self.a = [ax.clamp(-4.0e5, 4.0e5), ay.clamp(-4.0e5, 4.0e5)];
        self.a
    }
}

/// Verlet 绳链。
///
/// 质点用位置式 Verlet 积分，相邻点之间加距离约束，枢轴点每步钉死在 pivot。
/// 相比"刚性杆 + 单摆"，它能自然产生弯曲、下垂，并且对枢轴的上下抖动有真实响应
/// （竖直方向的参数共振会自己出现，不需要专门写）。
pub struct Rope {
    pub pts: Vec<Pt>,
    prev: Vec<Pt>,
    seg_len: f32,
    /// 重力加速度（px/s²，屏幕 y 向下）
    pub gravity: f32,
    /// 每秒速度保留比例对应的衰减系数，越大越黏
    pub damping: f32,
    /// 允许的最大偏角（弧度），超出会拉回锥形范围内
    pub max_theta: f32,
}

const ITERATIONS: usize = 4;

impl Rope {
    pub fn new(segments: usize, total_len: f32, pivot: Pt) -> Self {
        let n = segments.clamp(2, 30);
        let seg = (total_len / n as f32).max(1.0);
        let mut pts = Vec::with_capacity(n + 1);
        let mut prev = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let p = [pivot[0], pivot[1] + seg * i as f32];
            pts.push(p);
            prev.push(p);
        }
        Self {
            pts,
            prev,
            seg_len: seg,
            gravity: 2400.0,
            damping: 1.0,
            max_theta: 35f32.to_radians(),
        }
    }

    pub fn segments(&self) -> usize {
        self.pts.len() - 1
    }

    /// 改长度：保持当前形态，只重算段长并把点沿当前链重新分布。
    pub fn set_total_len(&mut self, total: f32) {
        let seg = (total / self.segments() as f32).max(1.0);
        self.seg_len = seg;
    }

    /// 推进一个固定步长。`pivot` 是窗口内坐标（窗口跟着光标走，所以枢轴在窗口里是固定的），
    /// `pivot_accel` 是光标加速度，用来施加非惯性系里的惯性力。
    pub fn step(&mut self, dt: f32, pivot: Pt, pivot_accel: Pt) {
        let dt2 = dt * dt;
        let keep = (-self.damping * dt).exp().clamp(0.0, 1.0);

        // 窗口随光标平移 → 在窗口坐标系里每个质点额外受 -a 的惯性力
        let gx = -pivot_accel[0];
        let gy = self.gravity - pivot_accel[1];

        for i in 1..self.pts.len() {
            let vx = (self.pts[i][0] - self.prev[i][0]) * keep;
            let vy = (self.pts[i][1] - self.prev[i][1]) * keep;
            self.prev[i] = self.pts[i];
            self.pts[i][0] += vx + gx * dt2;
            self.pts[i][1] += vy + gy * dt2;
        }

        self.pts[0] = pivot;
        self.prev[0] = pivot;

        for _ in 0..ITERATIONS {
            self.constrain(pivot);
        }
    }

    fn constrain(&mut self, pivot: Pt) {
        self.pts[0] = pivot;

        for i in 0..self.pts.len() - 1 {
            let d = sub(self.pts[i + 1], self.pts[i]);
            let dist = len(d).max(1e-4);
            let corr = (dist - self.seg_len) / dist * 0.5;
            let cx = d[0] * corr;
            let cy = d[1] * corr;

            if i > 0 {
                self.pts[i][0] += cx;
                self.pts[i][1] += cy;
            }
            self.pts[i + 1][0] -= cx;
            self.pts[i + 1][1] -= cy;
        }

        // 摆角限制：把每个质点拉回以枢轴为顶点的锥形范围内，
        // 保证画面不会超出按最大摆角算出来的窗口
        for i in 1..self.pts.len() {
            let dx = self.pts[i][0] - pivot[0];
            let dy = self.pts[i][1] - pivot[1];
            let ang = dx.atan2(dy);
            if ang.abs() > self.max_theta {
                let a = self.max_theta * ang.signum();
                let l = (dx * dx + dy * dy).sqrt();
                self.pts[i][0] = pivot[0] + a.sin() * l;
                self.pts[i][1] = pivot[1] + a.cos() * l;
            }
        }

        self.pts[0] = pivot;
    }

    pub fn tip(&self) -> Pt {
        *self.pts.last().unwrap()
    }

    /// 末端方向角（0 = 正下方，正 = 顺时针）。取末端两段的方向，避免单个点抖动。
    pub fn tip_angle(&self) -> f32 {
        let n = self.pts.len();
        let a = self.pts[n - 1];
        let b = self.pts[n.saturating_sub(3)];
        let dx = a[0] - b[0];
        let dy = a[1] - b[1];
        if dx * dx + dy * dy < 1e-6 {
            0.0
        } else {
            dx.atan2(dy)
        }
    }

    /// 是否已经静止（所有质点的每步位移都极小）
    pub fn is_at_rest(&self) -> bool {
        self.pts
            .iter()
            .zip(self.prev.iter())
            .skip(1)
            .all(|(p, q)| {
                let dx = p[0] - q[0];
                let dy = p[1] - q[1];
                dx * dx + dy * dy < 0.0004
            })
    }
}
