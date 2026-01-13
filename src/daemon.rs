use std::time::Duration;

use anyhow::{Context, bail};
use smithay_client_toolkit::reexports::client::EventQueue;
use tokio::{
    io::unix::AsyncFd,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};
use tracing::{debug, trace, warn};

use crate::{
    geometry_worker::{GeometryRequestId, GeometryWorker, GeometryWorkerEvent},
    gui::Gui,
    ipc::{AlttabwayIpc, IpcCommand},
    timer::Timer,
    wayland_client::{PreviewImage, WaylandClient, WaylandClientEvent},
    wgpu_wrapper::{WgpuSurface, WgpuWrapper},
};

#[derive(Debug)]
pub enum MaybeWgpuWrapper {
    Uninitialized,
    Initializing,
    Initialized(WgpuWrapper),
}

#[derive(Debug)]
enum DaemonEvent {
    WgpuSurface(WgpuWrapper, anyhow::Result<WgpuSurface>),
}

#[derive(Debug)]
pub struct Daemon {
    height: u32,
    width: u32,
    wgpu: Option<WgpuWrapper>,
    wgpu_surface: Option<WgpuSurface>,
    wayland_client: WaylandClient,
    wayland_client_q: EventQueue<WaylandClient>,
    wayland_client_rx: UnboundedReceiver<WaylandClientEvent>,

    command_tx: UnboundedSender<DaemonEvent>,
    command_rx: UnboundedReceiver<DaemonEvent>,
    gui: Gui,
    pending_repaint: bool,

    active_geometry_worker_request: Option<GeometryRequestId>,
    geometry_worker: GeometryWorker,
    geometry_worker_events: UnboundedReceiver<GeometryWorkerEvent>,

    ipc_listener: UnboundedReceiver<IpcCommand>,
    visible: bool,

    screenshot_timer: Timer,
}

impl Daemon {
    pub async fn start() -> anyhow::Result<()> {
        let ipc_listener = AlttabwayIpc::start_server().await?;
        let (geometry_worker, geometry_worker_events) = GeometryWorker::new()?;

        let wgpu_wrapper = WgpuWrapper::init().await?;

        let (wayland_client, wayland_client_q, wayland_client_rx) = WaylandClient::init()?;

        let (command_tx, command_rx) = mpsc::unbounded_channel();

        debug!("Initialized wayland layer client");

        let mut daemon = Self {
            height: 400,
            width: 800,
            wgpu: Some(wgpu_wrapper),
            wgpu_surface: None,
            wayland_client,
            wayland_client_q,
            wayland_client_rx,
            command_tx,
            command_rx,
            gui: Default::default(),
            pending_repaint: false,
            active_geometry_worker_request: None,
            geometry_worker,
            geometry_worker_events,
            ipc_listener,
            visible: false,
            screenshot_timer: Timer::new(Duration::from_secs(5)),
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
                            let width = if width == 0 { self.width } else { width };
                            let height = if height == 0 { self.height } else { height };

                            if !self.visible || self.wayland_client.surfaces.is_none() || self.wgpu_surface.is_some() {
                                continue;
                            }

                            if let Some(mut wgpu) = self.wgpu.take() {
                                let raw_handles = self.wayland_client.get_raw_handles()?;
                                let command_tx = self.command_tx.clone();


                                tokio::spawn(async move {
                                    let wgpu_surface = wgpu.init_surface(raw_handles, width, height);
                                    command_tx.send(DaemonEvent::WgpuSurface(wgpu, wgpu_surface)).unwrap();
                                });
                            }
                        }
                        WaylandClientEvent::Egui(events) => {
                            self.gui.handle_events(events);

                            if self.gui.needs_repaint() {
                                self.request_repaint()?
                            }
                        }
                        WaylandClientEvent::Frame => self.paint()?,
                        WaylandClientEvent::Hide => self.update_visibility(false)?,
                        WaylandClientEvent::Activate => {

                            if self.visible && self.wayland_client.surfaces.is_some() {
                                continue
                            }

                            self.screenshot_timer.ping_after(Duration::from_secs(1)).await?;
                        }
                        WaylandClientEvent::ModifierChange => {
                            if !self.wayland_client.get_modifiers().alt {
                                // TODO: activate window
                                self.update_visibility(false)?;
                            }
                        }
                    }
                },
                Some(event) = self.command_rx.recv() => {
                    trace!("received daemon event {:?}", event);

                    match event {
                        DaemonEvent::WgpuSurface(wgpu, wgpu_surface_result) => {
                            self.wgpu = wgpu.into();

                            match wgpu_surface_result {
                                Ok(wgpu_surface) => {
                                    self.width = wgpu_surface.surface_config.width;
                                    self.height = wgpu_surface.surface_config.height;
                                    self.wgpu_surface = Some(wgpu_surface);
                                    if self.visible {
                                        self.paint()?;
                                    } else {
                                        self.update_visibility(false)?;
                                    }
                                }
                                Err(err) => bail!(err)
                            }
                        }
                    }
                }
                result = self.geometry_worker_events.recv() => {
                    let event = result.context("geometry worker has crashed")?;
                    tracing::debug!("geometry worker event: {:?}", event);

                    match event {
                        GeometryWorkerEvent::ActiveWindow(request_id, geometry) => {
                            let Some(active_request_id) = self.active_geometry_worker_request else {
                                continue
                            };

                            if active_request_id != request_id {
                                continue
                            }

                            let (x, y, width, height) = geometry;

                            if width <= 0 || height <= 0 {
                                continue
                            }

                            let _ = self.wayland_client.capture_active_window_region(x, y, width, height, &self.wayland_client_q.handle());
                        }
                    }
                }
                result = self.ipc_listener.recv() => {
                    let event = result.context("ipc server has crashed")?;
                    tracing::debug!("ipc event: {:?}", event);

                    match event {
                        IpcCommand::Ping => (),
                        IpcCommand::Show => self.update_visibility(true)?,
                        IpcCommand::Hide => self.update_visibility(false)?,
                    }
                }
                result = self.screenshot_timer.wait() => {
                    result?;

                    let request_id = self.geometry_worker.request_active_window_geometry()?;
                    self.active_geometry_worker_request = Some(request_id);
                }
            }
        }
    }

    fn request_repaint(&mut self) -> anyhow::Result<()> {
        if self.pending_repaint {
            return Ok(());
        }
        self.pending_repaint = true;

        trace!("repaint requested");

        if let Some(surfaces) = &self.wayland_client.surfaces {
            surfaces
                .wl_surface
                .frame(&self.wayland_client_q.handle(), surfaces.wl_surface.clone());
            surfaces.wl_surface.commit();
        }
        Ok(())
    }

    fn update_visibility(&mut self, visible: bool) -> anyhow::Result<()> {
        self.visible = visible;

        if visible {
            // already visible
            if self.wayland_client.surfaces.is_some() {
                return Ok(());
            }

            self.wayland_client.create_surfaces(
                &self.wayland_client_q.handle(),
                self.width,
                self.height,
            )?;
            self.update_gui_items()?;
        } else {
            // we can't drop surfaces right now
            if self.wgpu.is_none() {
                return Ok(());
            }
            self.wgpu_surface.take();
            self.wayland_client.surfaces.take();
        }

        Ok(())
    }

    fn update_gui_items(&mut self) -> anyhow::Result<()> {
        self.gui.clear_items();
        for window in self.wayland_client.toplevel_windows.iter_mut() {
            if let Some(PreviewImage { buffer, is_rgba }) = &mut window.preview {
                warn!(
                    "has buffer {:?}",
                    &buffer
                        .canvas(&mut self.wayland_client.screenshot_pool)
                        .context("missing")?[0..4]
                );

                let canvas = buffer
                    .canvas(&mut self.wayland_client.screenshot_pool)
                    .context("missing canvas????")?;

                let rgba = if *is_rgba {
                    canvas
                } else {
                    *is_rgba = true;

                    for chunk in canvas.chunks_exact_mut(4) {
                        chunk.swap(0, 2);
                    }
                    canvas
                };

                let size = [(buffer.stride() / 4) as usize, buffer.height() as usize];

                let display_title = if window.title.is_empty() {
                    &window.app_id
                } else {
                    &window.title
                };

                self.gui.add_item(display_title, size, &rgba);
            }
        }

        self.request_repaint()?;

        Ok(())
    }

    fn paint(&mut self) -> anyhow::Result<()> {
        self.pending_repaint = false;

        if let (Some(wgpu), Some(wgpu_surface)) = (&mut self.wgpu, &mut self.wgpu_surface) {
            return self.gui.paint(wgpu, wgpu_surface);
        }
        warn!("paint requested but no surface?????");
        Ok(())
    }
}
