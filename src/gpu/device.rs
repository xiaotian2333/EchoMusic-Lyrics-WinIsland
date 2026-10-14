//! D3D12 设备与 Skia Ganesh D3D12 上下文（移植自 WinIsland 的 d3d12/mod.rs）。

use skia_safe::gpu::{
    self, ContextOptions, DirectContext, Protected, d3d::BackendContext,
    ganesh::context_options::Enable,
};
use windows62::Win32::Graphics::{
    Direct3D::D3D_FEATURE_LEVEL_11_0,
    Direct3D12::{
        D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC, D3D12CreateDevice,
        ID3D12CommandQueue, ID3D12Device,
    },
    DirectComposition::{DCompositionCreateDevice2, IDCompositionDevice},
    Dxgi::{
        CreateDXGIFactory2, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_CREATE_FACTORY_FLAGS,
        DXGI_ERROR_NOT_FOUND, IDXGIAdapter1, IDXGIFactory4,
    },
};

use super::RenderTarget;
use super::backdrop::SharedBackdrop;

const GPU_RESOURCE_CACHE_LIMIT: usize = 12 * 1024 * 1024;
const GPU_GLYPH_CACHE_LIMIT: usize = 2 * 1024 * 1024;

pub(crate) struct D3DDevice {
    pub(crate) context: DirectContext,
    pub(crate) composition: IDCompositionDevice,
    pub(crate) backend: BackendContext,
    pub(crate) factory: IDXGIFactory4,
}

impl D3DDevice {
    /// 枚举硬件适配器并创建 D3D12 设备与 Skia 上下文。
    pub(crate) fn new() -> Result<Self, String> {
        // SAFETY: DXGI 返回拥有的 COM 接口，不保留任何调用方指针。
        let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }
            .map_err(|error| format!("CreateDXGIFactory2 失败: {error}"))?;
        let mut errors = Vec::new();
        for index in 0.. {
            // SAFETY: 枚举基于存活的工厂，返回拥有的适配器接口。
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(format!("DXGI 适配器枚举失败: {error}")),
            };
            // SAFETY: 仅查询 DXGI 返回的存活适配器。
            let desc = unsafe { adapter.GetDesc1() }
                .map_err(|error| format!("DXGI 适配器描述失败: {error}"))?;
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let name = String::from_utf16_lossy(
                &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(128)],
            );
            match Self::from_adapter(factory.clone(), adapter) {
                Ok(device) => {
                    log::info!("D3D12 渲染设备: {name}");
                    return Ok(device);
                }
                Err(error) => errors.push(format!("{name}: {error}")),
            }
        }
        Err(format!(
            "没有可用的硬件 D3D12 适配器: {}",
            errors.join("; ")
        ))
    }

    fn from_adapter(factory: IDXGIFactory4, adapter: IDXGIAdapter1) -> Result<Self, String> {
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: 适配器存活，输出 Option 由本次调用初始化并拥有。
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }
            .map_err(|error| format!("D3D12CreateDevice 失败: {error}"))?;
        let device = device.ok_or_else(|| "D3D12CreateDevice 未返回设备".to_string())?;
        let desc = D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            ..Default::default()
        };
        // SAFETY: 描述符已初始化，返回的命令队列保留存活的设备。
        let queue = unsafe { device.CreateCommandQueue(&desc) }
            .map_err(|error| format!("D3D12 命令队列创建失败: {error}"))?;
        // SAFETY: DirectComposition 返回拥有的设备，无调用方资源。
        let composition: IDCompositionDevice = unsafe { DCompositionCreateDevice2(None) }
            .map_err(|error| format!("DirectComposition 设备创建失败: {error}"))?;
        let backend = BackendContext {
            adapter,
            device,
            queue,
            memory_allocator: None,
            protected_context: Protected::No,
        };
        let mut options = ContextOptions::new();
        options.glyph_cache_texture_maximum_bytes = GPU_GLYPH_CACHE_LIMIT;
        options.allow_multiple_glyph_cache_textures = Enable::No;
        options.reduce_ops_task_splitting = Enable::No;
        // SAFETY: backend 由本结构长期持有并晚于上下文释放，且两者只在渲染线程使用。
        let mut context = unsafe { DirectContext::new_d3d(&backend, Some(&options)) }
            .ok_or_else(|| "Skia D3D12 上下文创建失败".to_string())?;
        context.set_resource_cache_limit(GPU_RESOURCE_CACHE_LIMIT);
        Ok(Self {
            context,
            composition,
            backend,
            factory,
        })
    }

    /// 适配器 LUID，用于在捕获端创建同适配器的 D3D11 设备。
    pub(crate) fn adapter_luid(&self) -> u64 {
        // SAFETY: 仅查询保留的设备。
        let luid = unsafe { self.backend.device.GetAdapterLuid() };
        (u64::from(luid.HighPart as u32) << 32) | u64::from(luid.LowPart)
    }

    pub(crate) fn create_backdrop(
        &self,
        width: u32,
        height: u32,
        generation: u64,
    ) -> Result<SharedBackdrop, String> {
        SharedBackdrop::new(&self.backend.device, width, height, generation)
    }

    pub(crate) fn queue(&self) -> &ID3D12CommandQueue {
        &self.backend.queue
    }

    pub(crate) fn create_target(
        &mut self,
        hwnd_raw: isize,
        width: u32,
        height: u32,
    ) -> Result<RenderTarget, String> {
        self.check_health()?;
        self.validate_size(width, height)?;
        RenderTarget::new(self, hwnd_raw, width, height)
    }

    pub(crate) fn resize_target(
        &mut self,
        target: &mut RenderTarget,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if width == 0 || height == 0 || target.size() == (width, height) {
            return Ok(());
        }
        self.validate_size(width, height)?;
        self.synchronize()?;
        target.release_buffers();
        self.context
            .purge_unlocked_resources(gpu::PurgeResourceOptions::AllResources);
        self.synchronize()?;
        target.resize(&mut self.context, width, height)?;
        self.check_health()
    }

    fn validate_size(&self, width: u32, height: u32) -> Result<(), String> {
        let limit = self.context.max_render_target_size() as u32;
        if width == 0 || height == 0 || width > limit || height > limit {
            return Err(format!(
                "D3D12 目标尺寸非法 {width}x{height}（上限 {limit}）"
            ));
        }
        Ok(())
    }

    pub(crate) fn check_health(&mut self) -> Result<(), String> {
        // SAFETY: 仅查询保留 D3D12 设备的移除状态。
        unsafe { self.backend.device.GetDeviceRemovedReason() }
            .map_err(|error| format!("D3D12 设备已移除: {error}"))?;
        if self.context.abandoned() || self.context.is_device_lost() {
            return Err("Skia D3D12 上下文不可用".to_string());
        }
        if self.context.oomed() {
            return Err("Skia D3D12 分配失败".to_string());
        }
        Ok(())
    }

    pub(crate) fn submit(&mut self, sync: gpu::SyncCpu) -> Result<(), String> {
        if !self.context.submit(sync) {
            self.check_health()?;
            return Err("Skia D3D12 提交失败".to_string());
        }
        self.check_health()
    }

    pub(crate) fn synchronize(&mut self) -> Result<(), String> {
        self.check_health()?;
        self.context.flush(&gpu::FlushInfo::default());
        self.submit(gpu::SyncCpu::Yes)
    }
}

impl Drop for D3DDevice {
    fn drop(&mut self) {
        if self.synchronize().is_ok() {
            self.context.release_resources_and_abandon();
        } else {
            self.context.abandon();
        }
    }
}
