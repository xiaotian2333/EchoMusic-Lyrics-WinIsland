use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, Stream, StreamConfig};
use realfft::RealFftPlanner;
use tokio_util::sync::CancellationToken;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::{
    DEVICE_STATE, EDataFlow, ERole, Endpoints::IAudioMeterInformation, IAudioSessionControl2,
    IAudioSessionManager2, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
    MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
use windows::core::{Interface, PCWSTR, implement};

#[implement(IMMNotificationClient)]
struct AudioDeviceNotification {
    generation: Arc<AtomicUsize>,
}

#[allow(non_snake_case)]
impl IMMNotificationClient_Impl for AudioDeviceNotification_Impl {
    fn OnDeviceStateChanged(
        &self,
        _device_id: &PCWSTR,
        _new_state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _device_id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _device_id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _default_device_id: &PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender && role == eConsole {
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _device_id: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

pub struct AudioProcessor {
    spectrum: Arc<Mutex<[f32; 6]>>,
    gate: Arc<AtomicU32>,
    gate_override: Arc<AtomicU32>,
    device_generation: Arc<AtomicUsize>,
    cancel_token: CancellationToken,
}

impl AudioProcessor {
    pub fn new() -> Self {
        let spectrum = Arc::new(Mutex::new([0.0f32; 6]));
        let gate = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        // AtomicU32 stores f32 bit patterns since std::sync::atomic doesn't provide AtomicF32.
        // Relaxed ordering is sufficient: we only need eventual consistency for the gate value.
        let gate_override = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let device_generation = Arc::new(AtomicUsize::new(0));
        let cancel_token = CancellationToken::new();
        let processor = Self {
            spectrum,
            gate,
            gate_override,
            device_generation,
            cancel_token,
        };
        processor.start_device_watch_thread();
        processor.start_capture();
        processor.start_meter_thread();
        processor
    }

    pub fn get_spectrum(&self) -> [f32; 6] {
        *self.spectrum.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_gate_override(&self, value: bool) {
        let v = if value { 1.0f32 } else { 0.0f32 };
        self.gate_override.store(v.to_bits(), Ordering::Relaxed);
    }

    fn start_device_watch_thread(&self) {
        let cancel = self.cancel_token.clone();
        let generation = self.device_generation.clone();
        tokio::task::spawn_blocking(move || {
            while !cancel.is_cancelled() {
                // SAFETY: 当前阻塞任务独占此线程的 COM 单元，并使用多线程单元模型初始化。
                let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                if hr.is_err() {
                    log::error!("初始化音频设备通知 COM 失败: {hr:?}");
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }

                // SAFETY: COM 已在当前线程初始化，返回的枚举器仅在此线程内使用和释放。
                let enumerator = unsafe {
                    CoCreateInstance::<_, IMMDeviceEnumerator>(
                        &MMDeviceEnumerator,
                        None,
                        CLSCTX_ALL,
                    )
                };
                let enumerator = match enumerator {
                    Ok(enumerator) => enumerator,
                    Err(error) => {
                        log::error!("创建音频设备枚举器失败: {error}");
                        // SAFETY: 当前线程上的 COM 初始化已成功，且没有存活的 COM 对象。
                        unsafe {
                            CoUninitialize();
                        }
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                };

                let callback: IMMNotificationClient = AudioDeviceNotification {
                    generation: generation.clone(),
                }
                .into();
                // SAFETY: 枚举器和回调对象在注册期间持续存活，并会在释放前显式注销。
                let registration =
                    unsafe { enumerator.RegisterEndpointNotificationCallback(&callback) };
                if let Err(error) = registration {
                    log::error!("注册音频设备通知失败: {error}");
                    drop(callback);
                    drop(enumerator);
                    // SAFETY: 所有当前线程创建的 COM 对象均已释放。
                    unsafe {
                        CoUninitialize();
                    }
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }

                generation.fetch_add(1, Ordering::Relaxed);
                while !cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(100));
                }

                // SAFETY: 回调仍处于注册状态，枚举器与回调对象均有效。
                if let Err(error) =
                    unsafe { enumerator.UnregisterEndpointNotificationCallback(&callback) }
                {
                    log::warn!("注销音频设备通知失败: {error}");
                }
                drop(callback);
                drop(enumerator);
                // SAFETY: 所有当前线程创建的 COM 对象均已释放。
                unsafe {
                    CoUninitialize();
                }
            }
        });
    }

    fn start_meter_thread(&self) {
        let cancel = self.cancel_token.clone();
        let gate_clone = self.gate.clone();
        let device_generation = self.device_generation.clone();
        tokio::task::spawn_blocking(move || {
            // SAFETY: 当前阻塞任务独占此线程的 COM 单元，并使用多线程单元模型初始化。
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr.is_err() {
                gate_clone.store(0.0f32.to_bits(), Ordering::Relaxed);
                log::error!("初始化音量门控 COM 失败: {hr:?}");
                return;
            }

            while !cancel.is_cancelled() {
                gate_clone.store(0.0f32.to_bits(), Ordering::Relaxed);
                let observed_generation = device_generation.load(Ordering::Relaxed);
                // SAFETY: COM 已在当前线程初始化，创建的对象只在当前循环内使用。
                let session_manager: Option<IAudioSessionManager2> = unsafe {
                    (|| -> Option<IAudioSessionManager2> {
                        let enumerator: IMMDeviceEnumerator =
                            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
                        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
                        device.Activate(CLSCTX_ALL, None).ok()
                    })()
                };
                let Some(session_manager) = session_manager else {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                };

                while !cancel.is_cancelled()
                    && device_generation.load(Ordering::Relaxed) == observed_generation
                {
                    let mut max_peak = 0.0f32;
                    let mut should_rebuild = false;
                    // SAFETY: 会话及其测量接口均来自当前有效的会话管理器，且仅在本次迭代使用。
                    unsafe {
                        match session_manager.GetSessionEnumerator() {
                            Ok(enumerator) => match enumerator.GetCount() {
                                Ok(count) => {
                                    for i in 0..count {
                                        if let Ok(session) = enumerator.GetSession(i)
                                            && let Ok(session2) =
                                                session.cast::<IAudioSessionControl2>()
                                        {
                                            if session2.IsSystemSoundsSession() == S_OK {
                                                continue;
                                            }
                                            if let Ok(meter) =
                                                session.cast::<IAudioMeterInformation>()
                                                && let Ok(peak) = meter.GetPeakValue()
                                            {
                                                max_peak = max_peak.max(peak);
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    log::debug!("读取音频会话数量失败，准备重建: {error}");
                                    should_rebuild = true;
                                }
                            },
                            Err(error) => {
                                log::debug!("音频会话枚举失效，准备重建: {error}");
                                should_rebuild = true;
                            }
                        }
                    }
                    if should_rebuild {
                        break;
                    }
                    let gate_val = if max_peak > 0.002 { 1.0f32 } else { 0.0f32 };
                    gate_clone.store(gate_val.to_bits(), Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(50));
                }
                drop(session_manager);
                if !cancel.is_cancelled()
                    && device_generation.load(Ordering::Relaxed) == observed_generation
                {
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            gate_clone.store(0.0f32.to_bits(), Ordering::Relaxed);
            // SAFETY: 当前线程创建的所有 COM 对象均已释放。
            unsafe {
                CoUninitialize();
            }
        });
    }

    fn start_capture(&self) {
        let spectrum_arc = self.spectrum.clone();
        let cancel = self.cancel_token.clone();
        let gate_clone = self.gate.clone();
        let gate_override_clone = self.gate_override.clone();
        let device_generation = self.device_generation.clone();
        tokio::task::spawn_blocking(move || {
            // SAFETY: 当前阻塞任务独占此线程的 COM 单元，并使用多线程单元模型初始化。
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            let host = cpal::default_host();
            while !cancel.is_cancelled() {
                reset_spectrum(&spectrum_arc);
                let observed_generation = device_generation.load(Ordering::Relaxed);
                let Some(device) = host.default_output_device() else {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                };
                let config = match device.default_output_config() {
                    Ok(config) => config,
                    Err(error) => {
                        log::debug!("读取默认音频设备格式失败，准备重试: {error}");
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };
                let stream_config: StreamConfig = config.config();
                let stream_failed = Arc::new(AtomicBool::new(false));
                let stream = match config.sample_format() {
                    SampleFormat::F32 => build_capture_stream::<f32>(
                        &device,
                        &stream_config,
                        spectrum_arc.clone(),
                        gate_clone.clone(),
                        gate_override_clone.clone(),
                        stream_failed.clone(),
                    ),
                    SampleFormat::I16 => build_capture_stream::<i16>(
                        &device,
                        &stream_config,
                        spectrum_arc.clone(),
                        gate_clone.clone(),
                        gate_override_clone.clone(),
                        stream_failed.clone(),
                    ),
                    SampleFormat::U16 => build_capture_stream::<u16>(
                        &device,
                        &stream_config,
                        spectrum_arc.clone(),
                        gate_clone.clone(),
                        gate_override_clone.clone(),
                        stream_failed.clone(),
                    ),
                    sample_format => {
                        log::error!("不支持的默认音频采样格式: {sample_format:?}");
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };
                let stream = match stream {
                    Ok(stream) => stream,
                    Err(error) => {
                        log::debug!("创建频谱采集流失败，准备重试: {error}");
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };

                if let Err(error) = stream.play() {
                    log::debug!("启动频谱采集流失败，准备重试: {error}");
                    drop(stream);
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }

                while !cancel.is_cancelled()
                    && device_generation.load(Ordering::Relaxed) == observed_generation
                    && !stream_failed.load(Ordering::Relaxed)
                {
                    std::thread::sleep(Duration::from_millis(100));
                }

                drop(stream);
                reset_spectrum(&spectrum_arc);
                if !cancel.is_cancelled()
                    && device_generation.load(Ordering::Relaxed) == observed_generation
                    && stream_failed.load(Ordering::Relaxed)
                {
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            if hr.is_ok() {
                // SAFETY: 当前线程创建的所有 COM 对象均已释放。
                unsafe {
                    CoUninitialize();
                }
            }
        });
    }
}

fn build_capture_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    spectrum_arc: Arc<Mutex<[f32; 6]>>,
    gate_clone: Arc<AtomicU32>,
    gate_override_clone: Arc<AtomicU32>,
    stream_failed: Arc<AtomicBool>,
) -> Result<Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample + Copy,
    f32: FromSample<T>,
{
    let mut planner = RealFftPlanner::<f32>::new();
    let fft_len = 1024usize;
    let fft = planner.plan_fft_forward(fft_len);
    let mut output = fft.make_output_vec();
    let mut pcm_buffer = Vec::with_capacity(fft_len);
    let mut adaptive_max = [0.1f32; 6];

    device.build_input_stream(
        config,
        move |data: &[T], _: &_| {
            for &sample in data {
                pcm_buffer.push(f32::from_sample(sample));
                if pcm_buffer.len() >= fft_len {
                    update_spectrum(
                        &mut pcm_buffer,
                        &fft,
                        &mut output,
                        &mut adaptive_max,
                        &spectrum_arc,
                        &gate_clone,
                        &gate_override_clone,
                    );
                }
            }
        },
        move |error| {
            log::error!("频谱采集流发生错误: {error}");
            stream_failed.store(true, Ordering::Relaxed);
        },
        None,
    )
}

fn reset_spectrum(spectrum: &Arc<Mutex<[f32; 6]>>) {
    *spectrum.lock().unwrap_or_else(|error| error.into_inner()) = [0.0; 6];
}

fn update_spectrum(
    pcm_buffer: &mut Vec<f32>,
    fft: &Arc<dyn realfft::RealToComplex<f32>>,
    output: &mut [realfft::num_complex::Complex32],
    adaptive_max: &mut [f32; 6],
    spectrum_arc: &Arc<Mutex<[f32; 6]>>,
    gate_clone: &Arc<AtomicU32>,
    gate_override_clone: &Arc<AtomicU32>,
) {
    let fft_len = 1024;
    let mut indata = pcm_buffer[..fft_len].to_vec();
    pcm_buffer.drain(..fft_len);
    if let Err(e) = fft.process(&mut indata, output) {
        log::warn!("FFT processing failed: {:?}", e);
        // Feed the floor value into adaptive_max to prevent slow baseline decay
        // when FFT frames are intermittently dropped.
        for v in adaptive_max.iter_mut() {
            *v = *v * 0.995 + 0.01 * 0.005;
        }
        return;
    }
    let gate = f32::from_bits(gate_clone.load(Ordering::Relaxed));
    let gate_override = f32::from_bits(gate_override_clone.load(Ordering::Relaxed));
    let effective_gate = gate * gate_override;
    let mut raw_bins = [0.0f32; 6];
    let ranges = [(2, 8), (8, 20), (20, 50), (50, 120), (120, 280), (280, 511)];
    for (j, (start, end)) in ranges.iter().enumerate() {
        let mut sum = 0.0f32;
        sum += output[*start..*end].iter().map(|v| v.norm()).sum::<f32>();
        let avg = sum / (*end - *start) as f32;
        adaptive_max[j] = adaptive_max[j] * 0.995 + avg.max(0.01) * 0.005;
        raw_bins[j] = (avg / (adaptive_max[j] * 2.3) * effective_gate).clamp(0.0, 1.0);
    }
    let mut final_bins = [0.0f32; 6];
    final_bins[0] = raw_bins[5] * 0.8;
    final_bins[1] = raw_bins[3] * 0.9;
    final_bins[2] = raw_bins[0] * 1.0;
    final_bins[3] = raw_bins[1] * 1.0;
    final_bins[4] = raw_bins[2] * 0.9;
    final_bins[5] = raw_bins[4] * 0.8;
    if let Ok(mut s) = spectrum_arc.try_lock() {
        *s = final_bins;
    }
}

impl Drop for AudioProcessor {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}
