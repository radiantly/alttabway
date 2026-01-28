use std::{
    iter,
    sync::{Arc, Mutex},
};

use anyhow::bail;
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use smithay_client_toolkit::shm::slot::Buffer;
use tokio::sync::mpsc::UnboundedSender;
use wgpu::{Adapter, Backends, Instance, InstanceDescriptor, TextureFormat};

use crate::{
    gui::Gui,
    wayland_client::{RawHandles, WaylandClient},
};

pub trait Renderer {
    fn init_surface(
        &mut self,
        wayland_client: &mut WaylandClient,
        width: u32,
        height: u32,
        request_paint: UnboundedSender<()>,
    ) -> anyhow::Result<()>;
    fn destroy_surface(&mut self, wayland_client: &mut WaylandClient) -> anyhow::Result<()>;
    fn render(&mut self, wayland_client: &mut WaylandClient, gui: &mut Gui) -> anyhow::Result<()>;
}

struct WgpuState {
    instance: Instance,
    adapter: Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

struct WgpuSurface {
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
}

pub struct WgpuRenderer {
    state: Arc<WgpuState>,
    maybe_surface: Arc<Mutex<Option<WgpuSurface>>>,
    egui_renderer: Option<egui_wgpu::Renderer>,
}

impl WgpuRenderer {
    pub async fn new(backends: impl Into<Backends>) -> anyhow::Result<Self> {
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

        let state = WgpuState {
            instance,
            adapter,
            device,
            queue,
        };

        let renderer = Self {
            state: Arc::new(state),
            maybe_surface: Arc::new(Mutex::new(None)),
            egui_renderer: None,
        };

        tracing::info!("wgpu initialized successfully");
        Ok(renderer)
    }

    fn init_wgpu_surface(
        state: &WgpuState,
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

        let surface = unsafe { state.instance.create_surface_unsafe(target)? };

        let surface_caps = surface.get_capabilities(&state.adapter);
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

        surface.configure(&state.device, &config);

        Ok(WgpuSurface {
            surface,
            surface_config: config,
        })
    }
}

impl Renderer for WgpuRenderer {
    fn init_surface(
        &mut self,
        wayland_client: &mut WaylandClient,
        width: u32,
        height: u32,
        request_paint: UnboundedSender<()>,
    ) -> anyhow::Result<()> {
        let raw_handles = wayland_client.get_raw_handles()?;

        let state = self.state.clone();
        let maybe_surface = self.maybe_surface.clone();

        // TODO: Fix race condition that is a thing here
        tokio::spawn(async move {
            let Ok(wgpu_surface) = Self::init_wgpu_surface(&state, raw_handles, width, height)
            else {
                tracing::warn!("Critical error: failed to create wgpu surface.");
                return;
            };

            *maybe_surface.lock().unwrap() = Some(wgpu_surface);

            request_paint.send(()).unwrap();
        });
        Ok(())
    }

    fn destroy_surface(&mut self, _: &mut WaylandClient) -> anyhow::Result<()> {
        self.maybe_surface.lock().unwrap().take();
        Ok(())
    }

    fn render(&mut self, _: &mut WaylandClient, gui: &mut Gui) -> anyhow::Result<()> {
        let _span = tracing::trace_span!("Paint").entered();

        let Some(WgpuSurface {
            surface,
            surface_config,
        }) = &mut *self.maybe_surface.lock().unwrap()
        else {
            tracing::warn!("Render called but no surface!");
            return Ok(());
        };

        let output = surface.get_current_texture()?;

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let width = surface_config.width;
        let height = surface_config.height;

        let mut encoder =
            self.state
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Render Encoder"),
                });

        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point: 1.0,
        };

        let (textures_delta, clipped_primitives) = gui.get_output(width as f32, height as f32);

        tracing::trace!("Updating textures");

        let egui_renderer = self.egui_renderer.get_or_insert_with(|| {
            egui_wgpu::Renderer::new(
                &self.state.device,
                surface_config.format,
                egui_wgpu::RendererOptions::default(),
            )
        });

        for (id, image_delta) in &textures_delta.set {
            egui_renderer.update_texture(&self.state.device, &self.state.queue, *id, image_delta);
        }

        tracing::trace!("Updating buffers");

        egui_renderer.update_buffers(
            &self.state.device,
            &self.state.queue,
            &mut encoder,
            &clipped_primitives,
            &screen_descriptor,
        );

        {
            tracing::trace!("Beginning render pass");

            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            egui_renderer.render(
                &mut render_pass.forget_lifetime(),
                &clipped_primitives,
                &screen_descriptor,
            );
        }

        tracing::trace!("Freeing textures");
        for id in &textures_delta.free {
            egui_renderer.free_texture(id);
        }

        tracing::trace!("Submitting queue");
        self.state.queue.submit(iter::once(encoder.finish()));

        tracing::trace!("Presenting output");
        output.present();

        tracing::trace!("Completed");
        Ok(())
    }
}

pub struct SoftwareRenderer {
    buffer: Option<Buffer>,
    sw_render: EguiSoftwareRender,
}

impl SoftwareRenderer {
    pub fn new() -> Self {
        let sw_render = EguiSoftwareRender::new(ColorFieldOrder::Bgra);

        Self {
            buffer: None,
            sw_render,
        }
    }
}

// TODO: There's a bug with alpha on re-render. Investigate.
impl Renderer for SoftwareRenderer {
    fn init_surface(
        &mut self,
        wayland_client: &mut WaylandClient,
        width: u32,
        height: u32,
        request_paint: UnboundedSender<()>,
    ) -> anyhow::Result<()> {
        tracing::debug!("creating buffer");
        self.buffer = wayland_client
            .create_buffer(width as i32, height as i32)?
            .into();
        tracing::debug!("created buffer");
        request_paint.send(())?;
        Ok(())
    }

    fn destroy_surface(&mut self, _: &mut WaylandClient) -> anyhow::Result<()> {
        self.buffer.take();
        Ok(())
    }

    fn render(&mut self, wayland_client: &mut WaylandClient, gui: &mut Gui) -> anyhow::Result<()> {
        tracing::trace!("render requested!!");
        let Some(buffer) = &mut self.buffer else {
            bail!("missing buffer????");
        };

        let (width, height) = (buffer.stride() / 4, buffer.height());
        let (textures_delta, clipped_primitives) = gui.get_output(width as f32, height as f32);

        wayland_client.get_buffer_mut(buffer, |pixels| {
            // Transparency is not handled correctly if we do not reset the buffer
            pixels.fill(0);

            let (pixelbuf, _): (&mut [[u8; 4]], &mut [u8]) = pixels.as_chunks_mut();
            let mut buffer_ref = BufferMutRef::new(pixelbuf, width as usize, height as usize);

            self.sw_render
                .render(&mut buffer_ref, &clipped_primitives, &textures_delta, 1.0);
        });

        wayland_client.update_surface_buffer(buffer);
        tracing::trace!("render complete!!");
        Ok(())
    }
}
