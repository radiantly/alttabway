use std::{mem, time::Duration};

use anyhow::Context;
use smithay_client_toolkit::reexports::client::EventQueue;
use tokio::{
    io::unix::AsyncFd,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};
use tracing::{debug, trace};

use crate::{
    config_worker::{ConfigHandle, RenderBackend},
    geometry_worker::{GeometryWorker, GeometryWorkerEvent},
    gui::Gui,
    ipc::{AlttabwayIpc, Direction, IpcCommand, Modifier},
    renderer::{Renderer, SoftwareRenderer, WgpuRenderer},
    timer::Timer,
    wayland_client::WaylandClient,
    wayland_client_event::WaylandClientEvent,
};

use fast_image_resize::{PixelType, Resizer, images::Image};

pub struct Daemon {
    height: u32,
    width: u32,
    renderer: Box<dyn Renderer>,
    wayland_client: WaylandClient,
    wayland_client_q: EventQueue<WaylandClient>,
    wayland_client_rx: UnboundedReceiver<WaylandClientEvent>,

    renderer_tx: UnboundedSender<()>,
    renderer_rx: UnboundedReceiver<()>,
    gui: Gui,
    pending_repaint: bool,

    geometry_worker: GeometryWorker<u32>,

    ipc_listener: UnboundedReceiver<IpcCommand>,
    visible: bool,

    screenshot_timer: Timer,

    /// Modifier keys that are required to be pressed for the window to show
    required_modifiers: Vec<Modifier>,
}

impl Daemon {
    pub const DEFAULT_REQ_MODIFIER: [Modifier; 1] = [Modifier::Alt];

    pub async fn start() -> anyhow::Result<()> {
        // IPC Listener makes sure that this is the only instance running
        let ipc_listener = AlttabwayIpc::start_server().await?;
        let config_handle = ConfigHandle::new();
        let geometry_worker = GeometryWorker::new()?;

        let (wayland_client, wayland_client_q, wayland_client_rx) = WaylandClient::init()?;

        let (renderer_tx, renderer_rx) = mpsc::unbounded_channel();

        let renderer: Box<dyn Renderer> = match config_handle.get().render_backend {
            RenderBackend::Software => Box::new(SoftwareRenderer::new()),
            backends => Box::new(WgpuRenderer::new(backends).await?),
        };

        debug!("Initialized wayland layer client");

        let mut daemon = Self {
            height: 400,
            width: 800,
            renderer,
            wayland_client,
            wayland_client_q,
            wayland_client_rx,
            renderer_tx,
            renderer_rx,
            gui: Default::default(),
            pending_repaint: false,
            geometry_worker,
            ipc_listener,
            visible: false,
            screenshot_timer: Timer::new(Duration::from_secs(5)),
            required_modifiers: Self::DEFAULT_REQ_MODIFIER.to_vec(),
        };

        Daemon::run_loop(&mut daemon).await
    }

    async fn run_loop(&mut self) -> anyhow::Result<()> {
        loop {
            self.wayland_client_q.flush()?;

            let Some(read_guard) = self.wayland_client_q.prepare_read() else {
                self.wayland_client_q
                    .dispatch_pending(&mut self.wayland_client)?;
                continue;
            };

            let async_fd = AsyncFd::new(read_guard.connection_fd())?;

            tokio::select! {
                _ = async_fd.readable() => {
                    drop(async_fd);

                    read_guard.read()?;

                    self.wayland_client_q
                        .dispatch_pending(&mut self.wayland_client)?;
                },
                result = self.wayland_client_rx.recv() => {
                    let event = result.context("wayland client has crashed")?;
                    trace!("received wayland client event {:?}", event);

                    match event {
                        WaylandClientEvent::LayerShellConfigure(configure) => {
                            let (width, height) = configure.new_size;
                            self.width = if width == 0 { self.width } else { width };
                            self.height = if height == 0 { self.height } else { height };

                            if !self.visible || !self.wayland_client.has_surfaces() {
                                continue;
                            }

                            self.renderer.init_surface(&mut self.wayland_client, self.width, self.height, self.renderer_tx.clone())?;
                        }
                        WaylandClientEvent::Egui(events) => {
                            self.gui.handle_events(events);

                            if self.gui.needs_repaint() {
                                self.request_repaint()?
                            }
                        }
                        WaylandClientEvent::PaintRequest => self.paint()?,
                        WaylandClientEvent::ModifierChange => {
                            let wl_modifiers = self.wayland_client.get_modifiers();

                            if !wl_modifiers.ctrl && self.required_modifiers.contains(&Modifier::Ctrl) ||
                               !wl_modifiers.alt && self.required_modifiers.contains(&Modifier::Alt) ||
                               !wl_modifiers.shift && self.required_modifiers.contains(&Modifier::Shift) ||
                               !wl_modifiers.logo && self.required_modifiers.contains(&Modifier::Super)
                            {
                                if let Some(window_id) = self.gui.get_selected_item_id() {
                                    self.wayland_client.activate_window(window_id);
                                }

                                self.update_visibility(false)?;
                            }
                        }
                        WaylandClientEvent::TopLevelAdded(id) => self.gui.add_item(id),
                        WaylandClientEvent::TopLevelActivated(id) => {
                            self.gui.signal_item_activation(id);

                            // take screenshot for preview
                            if self.visible && self.wayland_client.has_surfaces() {
                                continue
                            }

                            self.screenshot_timer.ping_after(Duration::from_secs(1)).await?;
                        }
                        WaylandClientEvent::TopLevelTitleUpdate(id, new_title) => self.gui.update_item_title(id, new_title),
                        WaylandClientEvent::TopLevelAppIdUpdate(id, new_app_id) => self.gui.update_item_app_id(id, new_app_id),
                        WaylandClientEvent::TopLevelRemoved(id) => self.gui.remove_item(id),
                        WaylandClientEvent::ScreencopyDone(id, buffer) => {
                            // TODO: Resizing takes time, especially on slower computers
                            let _span = tracing::trace_span!("Resize", id=id).entered();
                            tracing::trace!("start");

                            let canvas = buffer
                                .canvas(self.wayland_client.get_screencopy_pool())
                                .context("missing canvas????")?;

                            let (width, height) = ((buffer.stride() / 4) as u32, buffer.height() as u32);
                            let (preview_width, preview_height) = self.gui.calculate_preview_size((width, height));

                            let mut dst_image = Image::new(preview_width, preview_height, PixelType::U8x4);
                            let src_image = Image::from_slice_u8(width, height, canvas, PixelType::U8x4)?;
                            let mut resizer = Resizer::new();
                            resizer.resize(&src_image, &mut dst_image, None)?;
                            tracing::trace!("resized");

                            let rgba = {
                                let mut bgra = dst_image.into_vec();
                                for chunk in bgra.chunks_exact_mut(4) {
                                    chunk.swap(0, 2);
                                }
                                bgra
                            };

                            self.gui.update_item_preview(id, &rgba, preview_width);
                            tracing::trace!("completed");
                        }
                    }
                },
                Some(()) = self.renderer_rx.recv() => {
                    self.paint()?
                }
                result = self.geometry_worker.recv() => {
                    let event = result.context("geometry worker has crashed")?;
                    tracing::debug!("geometry worker event: {:?}", event);

                    match event {
                        GeometryWorkerEvent::ActiveWindow(window_id, geometry) => {
                            let Some(active_window_id) = self.get_active_window_id() else {
                                continue
                            };

                            if active_window_id != window_id {
                                continue
                            }

                            if self.visible {
                                continue
                            }

                            let (x, y, width, height) = geometry;

                            if width <= 0 || height <= 0 {
                                continue
                            }

                            let _ = self.wayland_client.capture_window_region(window_id, x, y, width, height, &self.wayland_client_q.handle());
                        }
                    }
                }
                result = self.ipc_listener.recv() => {
                    let event = result.context("ipc server has crashed")?;
                    tracing::debug!("ipc event: {:?}", event);

                    match event {
                        IpcCommand::Ping => (),
                        IpcCommand::Show { direction, mut modifiers } => {
                            mem::swap(&mut self.required_modifiers, &mut modifiers);
                            if self.visible {
                                if let Some(direction) = direction {
                                    match direction {
                                        Direction::Previous => self.gui.select_previous_item(),
                                        Direction::Next => self.gui.select_next_item(),
                                    }
                                    self.request_repaint()?;
                                }
                            } else {
                                self.update_visibility(true)?;
                            }
                        }
                        IpcCommand::Hide => self.update_visibility(false)?,
                    }
                }
                result = self.screenshot_timer.wait() => {
                    result?;

                    let Some(active_window_id) = self.get_active_window_id() else { continue };

                    self.geometry_worker.request_active_window_geometry(active_window_id)?;
                }
            }
        }
    }

    fn get_active_window_id(&self) -> Option<u32> {
        self.gui.get_first_item_id()
    }

    fn request_repaint(&mut self) -> anyhow::Result<()> {
        if self.pending_repaint {
            return Ok(());
        }
        self.pending_repaint = true;

        trace!("repaint requested");
        self.wayland_client
            .request_paint(&self.wayland_client_q.handle());

        Ok(())
    }

    fn update_visibility(&mut self, visible: bool) -> anyhow::Result<()> {
        self.visible = visible;

        if visible {
            // already visible
            if self.wayland_client.has_surfaces() {
                return Ok(());
            }
            tracing::trace!("VISIBILITY CALLED");
            self.gui.reset_selected_item();

            (self.width, self.height) = self.gui.get_window_dimensions();

            self.wayland_client.create_surfaces(
                &self.wayland_client_q.handle(),
                self.width,
                self.height,
            )?;
            tracing::trace!("SURFACES CREATED");
        } else {
            self.renderer.destroy_surface(&mut self.wayland_client)?;
            self.wayland_client.destroy_surfaces();
        }

        Ok(())
    }

    fn paint(&mut self) -> anyhow::Result<()> {
        self.pending_repaint = false;

        tracing::trace!("PAINT COMPLETE");
        self.renderer
            .render(&mut self.wayland_client, &mut self.gui)?;

        // Update cursor icon
        use smithay_client_toolkit::seat::pointer::CursorIcon;
        match self.gui.get_cursor_icon() {
            egui::CursorIcon::Default => self.wayland_client.request_cursor(CursorIcon::Default),
            egui::CursorIcon::PointingHand => {
                self.wayland_client.request_cursor(CursorIcon::Pointer)
            }
            _ => (),
        }

        return Ok(());
    }
}
