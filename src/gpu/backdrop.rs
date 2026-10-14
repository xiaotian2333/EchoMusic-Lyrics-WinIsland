//! 共享背板纹理与 GPU frost 模糊管线（移植自 WinIsland 的 d3d12/backdrop.rs）。
//!
//! 渲染器在共享堆上创建 B8G8R8A8 纹理与读写围栏；捕获端把屏幕内容拷贝进来，
//! 渲染端经围栏同步后把它包装成 Skia 图像，并在 GPU 上完成降采样 + 可分离高斯模糊。

use std::cell::OnceCell;

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
            D3D12_FENCE_FLAG_SHARED, D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES,
            D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
            D3D12_RESOURCE_STATE_COMMON, D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12CommandQueue,
            ID3D12Device, ID3D12Fence, ID3D12Resource,
        },
        Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
    },
};

use super::{BackdropCaptureFrame, BackdropShare};

const SIZE_STEP: u32 = 64;
const MIN_FROST_SCALE: f32 = 2.0;
const MAX_FROST_SCALE: f32 = 24.0;
const MAX_WORK_SIGMA: f32 = 4.0;
const MIN_WORK_SIGMA: f32 = 0.3;
const MAX_WORK_SPAN: f32 = 12.0;

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

impl SharedBackdrop {
    pub(crate) fn new(
        device: &ID3D12Device,
        width: u32,
        height: u32,
        generation: u64,
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
        Ok(Self {
            resource,
            texture_handle,
            write,
            read,
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
