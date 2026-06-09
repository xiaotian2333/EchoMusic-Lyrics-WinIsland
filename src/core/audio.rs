#[cfg(windows)]
#[path = "audio_windows.rs"]
mod imp;

#[cfg(not(windows))]
mod imp {
    pub struct AudioProcessor;

    impl AudioProcessor {
        pub fn new() -> Self {
            Self
        }

        pub fn get_spectrum(&self) -> [f32; 6] {
            // TODO: 未来频谱数据会通过 WebSocket 下发；当前非 Windows 先返回空频谱。
            [0.0; 6]
        }

        pub fn set_gate_override(&self, _value: bool) {}
    }
}

pub use self::imp::*;
