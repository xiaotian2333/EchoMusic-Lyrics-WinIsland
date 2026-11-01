//! 共享背板纹理与 GPU frost 模糊管线（移植自 WinIsland 的 d3d12/backdrop.rs）。
//!
//! 渲染器在共享堆上创建 B8G8R8A8 纹理与读写围栏；捕获端把屏幕内容拷贝进来，
//! 渲染端经围栏同步后把它包装成 Skia 图像，并在 GPU 上完成降采样 + 可分离高斯模糊。

use std::cell::OnceCell;
use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use skia_safe::{
    AlphaType, BlendMode, ColorType, Data, FilterMode, ISize, Image, ImageInfo, MipmapMode, Paint,
    Rect, RuntimeEffect, SamplingOptions, Surface, TileMode,
    gpu::{self, DirectContext, SurfaceOrigin, d3d::TextureResourceInfo},
    runtime_effect::ChildPtr,
};
use windows62::Win32::{
    Foundation::{CloseHandle, GENERIC_ALL, HANDLE},
    Graphics::{
        Direct3D12::{
            D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_FENCE_FLAG_NONE, D3D12_FENCE_FLAG_SHARED,
            D3D12_HEAP_FLAG_NONE, D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES,
            D3D12_HEAP_TYPE_DEFAULT, D3D12_HEAP_TYPE_READBACK, D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
            D3D12_RANGE, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_BUFFER,
            D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS, D3D12_RESOURCE_FLAG_NONE,
            D3D12_RESOURCE_STATE_COMMON, D3D12_SUBRESOURCE_FOOTPRINT, D3D12_TEXTURE_COPY_LOCATION,
            D3D12_TEXTURE_COPY_LOCATION_0, D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX, D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12CommandAllocator, ID3D12CommandList,
            ID3D12CommandQueue, ID3D12Device, ID3D12Fence, ID3D12GraphicsCommandList,
            ID3D12Resource,
        },
        Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC},
    },
};

use super::{BackdropCaptureFrame, BackdropShare, CaptureBackend};
use crate::utils::liquid_glass::average_luminance_bgra;
use windows62::core::Interface;

const SIZE_STEP: u32 = 64;
const MIN_FROST_SCALE: f32 = 2.0;
const MAX_FROST_SCALE: f32 = 24.0;
const MAX_WORK_SIGMA: f32 = 4.0;
const MIN_WORK_SIGMA: f32 = 0.3;
const MAX_WORK_SPAN: f32 = 12.0;
/// 背板亮度采样的提交间隔；采样本身零等待，间隔只决定均值的新鲜度。
const LUMINANCE_TTL: Duration = Duration::from_millis(500);
/// 亮度回读的像素步进；双向隔 4 像素取样足以反映整体均值。
const LUMINANCE_STEP: usize = 4;

thread_local! {
    static FROST: OnceCell<Option<RuntimeEffect>> = const { OnceCell::new() };
}

/// 拥有并负责关闭的 NT 句柄。
struct OwnedHandle(HANDLE);

struct Fence {
    fence: ID3D12Fence,
    handle: OwnedHandle,
}

pub(crate) struct SharedBackdrop {
    resource: ID3D12Resource,
    texture_handle: OwnedHandle,
    write: Fence,
    read: Fence,
    luminance: Option<LuminanceSampler>,
    width: u32,
    height: u32,
    generation: u64,
    read_value: u64,
    frame: Option<BackdropCaptureFrame>,
    frost_sigma: f32,
    frost_surfaces: [Option<Surface>; 3],
    frost_scale: f32,
    images: Option<(u64, u32, Image, Image)>,
    used: bool,
}

/// 共享背板亮度采样器：渲染帧尾把背板异步拷入 READBACK 缓冲，下一帧收割。
///
/// 拷贝由自有命令列表提交到渲染队列，用自有围栏跨帧确认完成，全程不做 CPU
/// 等待——回读不再阻塞渲染帧（旧实现把 GPU 图像绘入 CPU 面，隐式回读会同步
/// 整条队列，空闲期表现为周期性掉帧）。拷贝在读围栏信号之前排队，捕获端覆写
/// 共享纹理前的围栏等待因此天然覆盖本读取，无读写竞态。
struct LuminanceSampler {
    texture: ID3D12Resource,
    readback: ID3D12Resource,
    fence: ID3D12Fence,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    /// 最近一次 Signal 的围栏值。
    value: u64,
    /// 已提交、尚未确认完成的拷贝对应的围栏值。
    pending: Option<u64>,
    width: u32,
    height: u32,
    row_pitch: u32,
    last_queued: Option<Instant>,
    luminance: Option<f32>,
    scratch: Vec<u8>,
    /// 提交链路出错后置位，采样永久停用（自动取色回退默认文字色）。
    broken: bool,
}

impl SharedBackdrop {
    pub(crate) fn new(
        device: &ID3D12Device,
        width: u32,
        height: u32,
        generation: u64,
        capture_backend: CaptureBackend,
    ) -> Result<Self, String> {
        let width = width.max(1).div_ceil(SIZE_STEP) * SIZE_STEP;
        let height = height.max(1).div_ceil(SIZE_STEP) * SIZE_STEP;
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: u64::from(width),
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
                | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
            ..Default::default()
        };
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: 两个描述符均已初始化，输出 Option 由本次调用初始化并拥有。
        unsafe {
            device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_SHARED,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )
        }
        .map_err(|error| format!("共享背板纹理创建失败: {error}"))?;
        let resource = resource.ok_or_else(|| "共享背板纹理缺失".to_string())?;
        // SAFETY: 资源创建于共享堆；返回的 NT 句柄由本结构拥有。
        let texture_handle = OwnedHandle(
            unsafe { device.CreateSharedHandle(&resource, None, GENERIC_ALL.0, None) }
                .map_err(|error| format!("共享背板句柄创建失败: {error}"))?,
        );
        let write = Fence::new(device)?;
        let read = Fence::new(device)?;
        // 亮度采样仅服务液态玻璃-DXGI 的自动取色；WGC 变体沿用 GDI 屏幕采样，不做 GPU 回读
        let luminance = if capture_backend == CaptureBackend::Dda {
            match LuminanceSampler::new(device, &resource, &desc, width, height) {
                Ok(sampler) => Some(sampler),
                Err(error) => {
                    log::warn!("背板亮度采样器创建失败，自动取色将停用: {error}");
                    None
                }
            }
        } else {
            None
        };
        Ok(Self {
            resource,
            texture_handle,
            write,
            read,
            luminance,
            width,
            height,
            generation,
            read_value: 0,
            frame: None,
            frost_sigma: 0.0,
            frost_surfaces: Default::default(),
            frost_scale: MIN_FROST_SCALE,
            images: None,
            used: false,
        })
    }

    /// 纹理是否仍可覆盖指定尺寸（尺寸超出或余量过大时需重建）。
    pub(crate) fn fits(&self, width: u32, height: u32) -> bool {
        width <= self.width
            && height <= self.height
            && self.width - width < SIZE_STEP * 4
            && self.height - height < SIZE_STEP * 4
    }

    pub(crate) fn share(&self) -> BackdropShare {
        BackdropShare {
            texture: self.texture_handle.0.0 as usize,
            write_fence: self.write.handle.0.0 as usize,
            read_fence: self.read.handle.0.0 as usize,
            width: self.width,
            height: self.height,
            generation: self.generation,
        }
    }

    pub(crate) fn read_value(&self) -> u64 {
        self.read_value
    }

    /// 提交并收割一次背板亮度采样；每渲染帧调用，全程不做 CPU 等待。
    pub(crate) fn queue_luminance_sample(&mut self, queue: &ID3D12CommandQueue) {
        if let Some(sampler) = self.luminance.as_mut() {
            sampler.queue(queue);
        }
    }

    /// 最近一次采样到的岛屿下方内容平均亮度（0.0~1.0）；尚未产出时为 None。
    pub(crate) fn luminance(&self) -> Option<f32> {
        self.luminance
            .as_ref()
            .and_then(|sampler| sampler.luminance)
    }

    /// 记录本帧的捕获结果；新写入值出现时让 GPU 等待捕获拷贝完成。
    pub(crate) fn set_frame(
        &mut self,
        queue: &ID3D12CommandQueue,
        frame: Option<BackdropCaptureFrame>,
        frost_sigma: f32,
    ) -> Result<(), String> {
        if let Some(frame) = frame
            && self
                .frame
                .is_none_or(|previous| previous.write_value != frame.write_value)
        {
            // SAFETY: 围栏存活；队列在 GPU 上等待捕获端的拷贝完成。
            unsafe { queue.Wait(&self.write.fence, frame.write_value) }
                .map_err(|error| format!("背板围栏等待失败: {error}"))?;
        }
        self.frame = frame;
        self.frost_sigma = frost_sigma;
        Ok(())
    }

    /// 本帧采样结束后向捕获端发读完成信号，允许其覆写共享纹理。
    pub(crate) fn finish_frame(&mut self, queue: &ID3D12CommandQueue) -> Result<(), String> {
        if !std::mem::take(&mut self.used) {
            return Ok(());
        }
        let value = self.read_value + 1;
        // SAFETY: 围栏存活；信号发生在全部已提交的纹理读取之后。
        unsafe { queue.Signal(&self.read.fence, value) }
            .map_err(|error| format!("背板围栏信号失败: {error}"))?;
        self.read_value = value;
        Ok(())
    }

    /// 返回当前帧的原始屏幕内容与其模糊副本。
    pub(crate) fn images(&mut self, context: &mut DirectContext) -> Option<(Image, Image)> {
        let serial = self.frame?.serial;
        let sigma_key = (self.frost_sigma * 16.0).round() as u32;
        if let Some((cached, key, sharp, blurred)) = &self.images
            && *cached == serial
            && *key == sigma_key
        {
            self.used = true;
            return Some((sharp.clone(), blurred.clone()));
        }
        let info = TextureResourceInfo {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            level_count: 1,
            ..TextureResourceInfo::from_resource(self.resource.clone())
                .with_state(D3D12_RESOURCE_STATE_COMMON)
        };
        let backend = gpu::BackendTexture::new_d3d((self.width as i32, self.height as i32), &info);
        let sharp = gpu::images::borrow_texture_from(
            context,
            &backend,
            SurfaceOrigin::TopLeft,
            ColorType::BGRA8888,
            AlphaType::Opaque,
            None,
        )?;
        self.used = true;
        self.images = None;
        let blurred = self.render_frost(context, &sharp)?;
        self.images = Some((serial, sigma_key, sharp.clone(), blurred.clone()));
        Some((sharp, blurred))
    }

    /// 背板相对原始分辨率的分辨率比例；1.0 表示未模糊。
    pub(crate) fn frost_scale(&self) -> f32 {
        self.frost_scale
    }

    fn render_frost(&mut self, context: &mut DirectContext, sharp: &Image) -> Option<Image> {
        if self.frost_sigma < MIN_WORK_SIGMA {
            self.frost_surfaces = Default::default();
            self.frost_scale = 1.0;
            return Some(sharp.clone());
        }
        let effect = FROST.with(|cell| {
            cell.get_or_init(|| {
                RuntimeEffect::make_for_shader(include_str!("../utils/frost.sksl"), None)
                    .map_err(|error| log::error!("背板模糊着色器编译失败: {error}"))
                    .ok()
            })
            .clone()
        })?;
        let scale = (self.frost_sigma / MAX_WORK_SIGMA)
            .ceil()
            .clamp(MIN_FROST_SCALE, MAX_FROST_SCALE);
        let size = ISize::new(
            (sharp.width() as f32 / scale).ceil() as i32,
            (sharp.height() as f32 / scale).ceil() as i32,
        );
        for surface in &mut self.frost_surfaces {
            if surface.as_ref().is_none_or(|surface| {
                surface.width() != size.width || surface.height() != size.height
            }) {
                let info = ImageInfo::new(size, ColorType::RGBA8888, AlphaType::Premul, None);
                *surface = gpu::surfaces::render_target(
                    context,
                    gpu::Budgeted::Yes,
                    &info,
                    None,
                    Some(SurfaceOrigin::TopLeft),
                    None,
                    Some(false),
                    Some(false),
                );
            }
        }
        self.frost_scale = scale;
        let sigma = self.frost_sigma / scale;
        let reduced = frost_pass(
            context,
            &effect,
            sharp,
            self.frost_surfaces[0].as_mut()?,
            FrostPass::Reduce(scale),
        )?;
        if sigma < MIN_WORK_SIGMA {
            return Some(reduced);
        }
        let horizontal = frost_pass(
            context,
            &effect,
            &reduced,
            self.frost_surfaces[1].as_mut()?,
            FrostPass::Blur([1.0, 0.0], sigma),
        )?;
        drop(reduced);
        frost_pass(
            context,
            &effect,
            &horizontal,
            self.frost_surfaces[2].as_mut()?,
            FrostPass::Blur([0.0, 1.0], sigma),
        )
    }
}

impl LuminanceSampler {
    fn new(
        device: &ID3D12Device,
        texture: &ID3D12Resource,
        desc: &D3D12_RESOURCE_DESC,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let allocator: ID3D12CommandAllocator =
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT) }
                .map_err(|error| format!("亮度采样命令分配器创建失败: {error}"))?;
        // CreateCommandList 返回的列表处于录制状态，先关闭以统一走 Reset 提交路径
        let list: ID3D12GraphicsCommandList = unsafe {
            device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
        }
        .map_err(|error| format!("亮度采样命令列表创建失败: {error}"))?;
        // SAFETY: 列表为空且处于录制状态，仅关闭以备后续重置。
        unsafe { list.Close() }.map_err(|error| format!("亮度采样命令列表关闭失败: {error}"))?;
        let fence: ID3D12Fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }
            .map_err(|error| format!("亮度采样围栏创建失败: {error}"))?;
        let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut rows = 0_u32;
        let mut row_size = 0_u64;
        let mut total_bytes = 0_u64;
        // SAFETY: 描述符存活，四个输出指针均指向本次调用初始化的局部变量。
        unsafe {
            device.GetCopyableFootprints(
                desc,
                0,
                1,
                0,
                Some(&mut footprint),
                Some(&mut rows),
                Some(&mut row_size),
                Some(&mut total_bytes),
            )
        };
        let readback_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: total_bytes,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_UNKNOWN,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_NONE,
        };
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_READBACK,
            ..Default::default()
        };
        let mut readback: Option<ID3D12Resource> = None;
        // SAFETY: 两个描述符均已初始化，输出 Option 由本次调用初始化并拥有。
        unsafe {
            device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &readback_desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut readback,
            )
        }
        .map_err(|error| format!("亮度回读缓冲创建失败: {error}"))?;
        Ok(Self {
            texture: texture.clone(),
            readback: readback.ok_or_else(|| "亮度回读缓冲缺失".to_string())?,
            fence,
            allocator,
            list,
            value: 0,
            pending: None,
            width,
            height,
            row_pitch: footprint.Footprint.RowPitch,
            last_queued: None,
            luminance: None,
            scratch: Vec::new(),
            broken: false,
        })
    }

    /// 收割已完成的拷贝并按 TTL 提交新拷贝；调用点在渲染帧的 Skia 提交之后。
    fn queue(&mut self, queue: &ID3D12CommandQueue) {
        if self.broken {
            return;
        }
        // 围栏确认完成后才映射，Map 本身零等待
        if let Some(pending) = self.pending
            && unsafe { self.fence.GetCompletedValue() } >= pending
        {
            self.pending = None;
            self.resolve();
        }
        if self.pending.is_some()
            || self
                .last_queued
                .is_some_and(|at| at.elapsed() < LUMINANCE_TTL)
        {
            return;
        }
        self.submit(queue);
    }

    /// 把共享背板整幅拷入 READBACK 缓冲并按队列顺序信号围栏。
    fn submit(&mut self, queue: &ID3D12CommandQueue) {
        // SAFETY: 仅在上一轮拷贝的围栏确认完成后才走到这里（pending 已清空），
        // 分配器与命令列表不被 GPU 占用，可安全重置。
        if let Err(error) = unsafe {
            self.allocator
                .Reset()
                .and_then(|()| self.list.Reset(&self.allocator, None))
        } {
            log::warn!("亮度采样命令列表重置失败，采样停用: {error}");
            self.broken = true;
            return;
        }
        let mut dst = D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(self.readback.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                    Offset: 0,
                    Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        Width: self.width,
                        Height: self.height,
                        Depth: 1,
                        RowPitch: self.row_pitch,
                    },
                },
            },
        };
        let mut src = D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(self.texture.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
            },
        };
        // SAFETY: 拷贝位置覆盖两个资源的全部子资源；READBACK 缓冲经隐式提升进入
        // COPY_DEST，共享纹理带 ALLOW_SIMULTANEOUS_ACCESS 且处于 COMMON，可直作拷贝源。
        unsafe { self.list.CopyTextureRegion(&dst, 0, 0, 0, &src, None) };
        // 拷贝位置中的 COM 引用经 ManuallyDrop 持有，FFI 不接管，须手动释放
        // SAFETY: pResource 已在上方拷贝调用中使用完毕。
        unsafe { ManuallyDrop::drop(&mut dst.pResource) };
        // SAFETY: 同上。
        unsafe { ManuallyDrop::drop(&mut src.pResource) };
        // SAFETY: 命令列表已录制完毕且处于关闭状态。
        if let Err(error) = unsafe { self.list.Close() } {
            log::warn!("亮度采样命令列表关闭失败，采样停用: {error}");
            self.broken = true;
            return;
        }
        let Ok(command_list) = self.list.cast::<ID3D12CommandList>() else {
            log::warn!("亮度采样命令列表转换失败，采样停用");
            self.broken = true;
            return;
        };
        // SAFETY: 队列按提交顺序执行，拷贝先于后续信号完成。
        unsafe { queue.ExecuteCommandLists(&[Some(command_list)]) };
        self.value += 1;
        // SAFETY: 信号排在拷贝之后入队，触发即代表回读缓冲就绪。
        if let Err(error) = unsafe { queue.Signal(&self.fence, self.value) } {
            log::warn!("亮度采样围栏信号失败，采样停用: {error}");
            self.broken = true;
            return;
        }
        self.pending = Some(self.value);
        self.last_queued = Some(Instant::now());
    }

    /// 映射回读缓冲，按步进取样计算 BT.601 平均亮度后解除映射。
    fn resolve(&mut self) {
        let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: 围栏已确认拷贝完成；空 RANGE 表示只读映射。
        if let Err(error) = unsafe {
            self.readback
                .Map(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }), Some(&mut data))
        } {
            log::warn!("亮度回读缓冲映射失败，采样停用: {error}");
            self.broken = true;
            return;
        }
        // SAFETY: 映射行仅在行距与宽高范围内读取，读取完成后立即解除映射。
        unsafe {
            let base = data.cast::<u8>();
            self.scratch.clear();
            for y in (0..self.height as usize).step_by(LUMINANCE_STEP) {
                let row = base.add(y * self.row_pitch as usize);
                for x in (0..self.width as usize).step_by(LUMINANCE_STEP) {
                    let pixel = row.add(x * 4);
                    self.scratch
                        .extend_from_slice(std::slice::from_raw_parts(pixel, 4));
                }
            }
            self.readback
                .Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
        }
        self.luminance = Some(average_luminance_bgra(&self.scratch));
    }
}

enum FrostPass {
    /// 降采样（盒式平均）。
    Reduce(f32),
    /// 可分离高斯的一遍，参数为方向与工作分辨率下的 sigma。
    Blur([f32; 2], f32),
}

fn frost_pass(
    context: &mut DirectContext,
    effect: &RuntimeEffect,
    input: &Image,
    target: &mut Surface,
    pass: FrostPass,
) -> Option<Image> {
    let source = input.to_shader(
        (TileMode::Clamp, TileMode::Clamp),
        SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
        None,
    )?;
    let mut uniforms = vec![0_u8; effect.uniform_size()];
    let mut set = |name: &str, values: &[f32]| {
        if let Some(uniform) = effect.find_uniform(name) {
            for (index, value) in values.iter().enumerate() {
                let offset = uniform.offset() + index * 4;
                uniforms[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
            }
        }
    };
    match pass {
        FrostPass::Reduce(scale) => set("boxSize", &[scale]),
        FrostPass::Blur(direction, sigma) => {
            set("direction", &direction);
            set("sigma", &[sigma]);
            set("span", &[(sigma * 3.0).ceil().min(MAX_WORK_SPAN)]);
        }
    }
    let shader = effect.make_shader(Data::new_copy(&uniforms), &[ChildPtr::from(source)], None)?;
    let mut paint = Paint::default();
    paint.set_shader(shader);
    paint.set_blend_mode(BlendMode::Src);
    let target_w = target.width() as f32;
    let target_h = target.height() as f32;
    let bounds = Rect::from_xywh(0.0, 0.0, target_w, target_h);
    target.canvas().draw_rect(bounds, &paint);
    context.flush_surface(target);
    Some(target.image_snapshot())
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: NT 句柄由本所有者创建，且只关闭一次。
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl Fence {
    fn new(device: &ID3D12Device) -> Result<Self, String> {
        // SAFETY: 设备存活，返回创建于共享堆的拥有围栏。
        let fence: ID3D12Fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_SHARED) }
            .map_err(|error| format!("共享背板围栏创建失败: {error}"))?;
        // SAFETY: 围栏可共享；返回的 NT 句柄由本值拥有。
        let handle = OwnedHandle(
            unsafe { device.CreateSharedHandle(&fence, None, GENERIC_ALL.0, None) }
                .map_err(|error| format!("共享背板围栏句柄创建失败: {error}"))?,
        );
        Ok(Self { fence, handle })
    }
}
