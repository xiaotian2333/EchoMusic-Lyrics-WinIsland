//! DirectComposition 合成交换链渲染目标（移植自 WinIsland 的 d3d12/target.rs）。

use skia_safe::{
    ColorType, Surface,
    gpu::{self, DirectContext, SurfaceOrigin, d3d::TextureResourceInfo},
};
use windows62::Win32::{
    Foundation::HWND,
    Graphics::{
        Direct3D12::D3D12_RESOURCE_STATE_PRESENT,
        DirectComposition::{IDCompositionDevice, IDCompositionTarget, IDCompositionVisual},
        Dxgi::{
            Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
            DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
            DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGISwapChain3,
        },
    },
};
use windows62::core::{IUnknown, Interface};

use super::D3DDevice;

const BUFFER_COUNT: u32 = 2;

pub(crate) struct RenderTarget {
    surfaces: Vec<Surface>,
    visual: IDCompositionVisual,
    target: IDCompositionTarget,
    swap_chain: IDXGISwapChain3,
    composition: IDCompositionDevice,
    width: u32,
    height: u32,
}

impl RenderTarget {
    /// 在窗口上创建合成交换链并挂载 DirectComposition 视觉树。
    pub(crate) fn new(
        device: &mut D3DDevice,
        hwnd_raw: isize,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: BUFFER_COUNT,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            ..Default::default()
        };
        // SAFETY: 描述符已初始化，保留的 DIRECT 队列满足合成交换链要求。
        let swap_chain: IDXGISwapChain3 = unsafe {
            device
                .factory
                .CreateSwapChainForComposition(&device.backend.queue, &desc, None)
        }
        .and_then(|swap_chain| swap_chain.cast())
        .map_err(|error| format!("合成交换链创建失败: {error}"))?;
        let surfaces = wrap_buffers(&swap_chain, &mut device.context, width, height)?;
        // SAFETY: 窗口由应用长期持有；每个窗口只创建一个前台合成目标。
        let target = unsafe {
            device
                .composition
                .CreateTargetForHwnd(HWND(hwnd_raw as *mut _), true)
        }
        .map_err(|error| format!("DirectComposition 目标创建失败: {error}"))?;
        // SAFETY: 合成设备存活，返回拥有的视觉对象。
        let visual = unsafe { device.composition.CreateVisual() }
            .map_err(|error| format!("DirectComposition 视觉对象创建失败: {error}"))?;
        let target = Self {
            surfaces,
            visual,
            target,
            swap_chain,
            composition: device.composition.clone(),
            width,
            height,
        };
        // SAFETY: 所有接口由 target 保留；即使挂载失败也会在 Drop 中分离。
        unsafe {
            target
                .visual
                .SetContent(&target.swap_chain)
                .and_then(|()| target.target.SetRoot(&target.visual))
                .and_then(|()| target.composition.Commit())
        }
        .map_err(|error| format!("DirectComposition 挂载失败: {error}"))?;
        Ok(target)
    }

    pub(crate) fn current_surface(&mut self) -> Result<&mut Surface, String> {
        // SAFETY: 交换链由本结构保留，且只在渲染线程访问。
        let index = unsafe { self.swap_chain.GetCurrentBackBufferIndex() } as usize;
        self.surfaces
            .get_mut(index)
            .ok_or_else(|| "DXGI 后台缓冲不可用".to_string())
    }

    pub(crate) fn present(&self) -> Result<(), String> {
        // SAFETY: 渲染器已用 Present 访问刷新当前表面，并在该交换链队列上提交。
        unsafe { self.swap_chain.Present(1, DXGI_PRESENT(0)).ok() }
            .map_err(|error| format!("DXGI 呈现失败: {error}"))
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 释放 Skia 对后台缓冲的包装引用（重建尺寸前调用）。
    pub(crate) fn release_buffers(&mut self) {
        self.surfaces.clear();
    }

    pub(crate) fn resize(
        &mut self,
        context: &mut DirectContext,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        // SAFETY: D3DDevice 已同步队列并释放全部 Skia 后台缓冲引用。
        unsafe {
            self.swap_chain.ResizeBuffers(
                BUFFER_COUNT,
                width,
                height,
                DXGI_FORMAT_R8G8B8A8_UNORM,
                Default::default(),
            )
        }
        .map_err(|error| format!("DXGI 缓冲重建失败: {error}"))?;
        self.surfaces = wrap_buffers(&self.swap_chain, context, width, height)?;
        self.width = width;
        self.height = height;
        Ok(())
    }
}

impl Drop for RenderTarget {
    fn drop(&mut self) {
        // SAFETY: 分离期间合成接口与窗口均存活。
        unsafe {
            let root_result = self.target.SetRoot(None::<&IDCompositionVisual>);
            let content_result = self.visual.SetContent(None::<&IUnknown>);
            let commit_result = self
                .composition
                .Commit()
                .and_then(|()| self.composition.WaitForCommitCompletion());
            if let Err(error) = root_result.and(content_result).and(commit_result) {
                log::warn!("DirectComposition 分离失败: {error}");
            }
        }
    }
}

fn wrap_buffers(
    swap_chain: &IDXGISwapChain3,
    context: &mut DirectContext,
    width: u32,
    height: u32,
) -> Result<Vec<Surface>, String> {
    (0..BUFFER_COUNT)
        .map(|index| {
            // SAFETY: index 在交换链缓冲数量内，返回的资源由本次调用拥有。
            let resource = unsafe { swap_chain.GetBuffer(index) }
                .map_err(|error| format!("DXGI 缓冲 {index} 获取失败: {error}"))?;
            let info = TextureResourceInfo {
                format: DXGI_FORMAT_R8G8B8A8_UNORM,
                level_count: 1,
                ..TextureResourceInfo::from_resource(resource)
                    .with_state(D3D12_RESOURCE_STATE_PRESENT)
            };
            let backend = gpu::BackendRenderTarget::new_d3d((width as i32, height as i32), &info);
            gpu::surfaces::wrap_backend_render_target(
                context,
                &backend,
                SurfaceOrigin::TopLeft,
                ColorType::RGBA8888,
                None,
                None,
            )
            .ok_or_else(|| format!("Skia D3D12 缓冲 {index} 包装失败"))
        })
        .collect()
}
