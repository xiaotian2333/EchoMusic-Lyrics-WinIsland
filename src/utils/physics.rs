pub struct Spring {
    pub value: f32,
    pub velocity: f32,
}
impl Spring {
    pub fn new(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
        }
    }
    pub fn update_dt(&mut self, target: f32, stiffness: f32, damping: f32, dt: f32) {
        let force = (target - self.value) * stiffness * dt;
        self.velocity = (self.velocity + force) * damping.powf(dt);
        self.value += self.velocity * dt;
    }
}

/// 把按 60FPS 单帧调的指数平滑系数换算为时间无关系数：
/// 高刷新率下每帧推进更少，动画总速与刷新率无关。
pub fn dt_smoothing_factor(base: f32, dt: f32) -> f32 {
    1.0 - (1.0 - base).powf(dt.max(0.01))
}
