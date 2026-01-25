use wgpu::{Adapter, Backends, Instance, InstanceDescriptor, TextureFormat};

use crate::wayland_client::RawHandles;
use std::fmt;
pub struct WgpuWrapper {
    instance: Instance,
    adapter: Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl WgpuWrapper {
    pub async fn init(backends: impl Into<Backends>) -> anyhow::Result<Self> {
        // Initialize wgpu

        let instance = wgpu::Instance::new(&InstanceDescriptor {
            backends: backends.into(),
            ..Default::default()
        });

        tracing::debug!("requesting adapter...");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: None, // Note: may possibly need to pass surface here. lazy init if this causes problems down the line
                force_fallback_adapter: false,
            })
            .await?;

        let adapter_info = adapter.get_info();
        tracing::info!(
            "Adapter [{}][{}] acquired.",
            adapter_info.name,
            adapter_info.backend
        );
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_features: wgpu::Features::default(),
                required_limits: wgpu::Limits::default(),
                label: None,
                memory_hints: wgpu::MemoryHints::default(),
                experimental_features: wgpu::ExperimentalFeatures::default(),
                trace: Default::default(),
            })
            .await?;

        tracing::debug!("device aquired, continuing...");

        let wgpu_wrapper = Self {
            instance,
            adapter,
            device,
            queue,
        };

        tracing::info!("wgpu initialized successfully");
        Ok(wgpu_wrapper)
    }

    pub fn init_surface(
        &mut self,
        raw_handles: RawHandles,
        width: u32,
        height: u32,
    ) -> anyhow::Result<WgpuSurface> {
        let RawHandles {
            raw_display_handle,
            raw_window_handle,
        } = raw_handles;

        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle,
            raw_window_handle,
        };

        let surface = unsafe { self.instance.create_surface_unsafe(target)? };

        let surface_caps = surface.get_capabilities(&self.adapter);
        tracing::debug!("caps: {:?}", surface_caps);

        let surface_format = match surface_caps.formats.contains(&TextureFormat::Rgba8Unorm) {
            true => TextureFormat::Rgba8Unorm,
            false => surface_caps.formats[0],
        };

        let alpha_mode = match surface_caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            true => wgpu::CompositeAlphaMode::PreMultiplied,
            false => surface_caps.alpha_modes[0],
        };

        tracing::debug!("using format {:?} {:?}", surface_format, alpha_mode);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&self.device, &config);

        Ok(WgpuSurface {
            surface,
            surface_config: config,
        })
    }
}

// we need this because egui_wgpu::Renderer doesn't implement Debug ugh
impl fmt::Debug for WgpuWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WgpuWrapper")
            .field("device", &self.device)
            .field("queue", &self.queue)
            .finish()
    }
}

#[derive(Debug)]
pub struct WgpuSurface {
    pub surface: wgpu::Surface<'static>,
    pub surface_config: wgpu::SurfaceConfiguration,
}
