use std::thread;
use std::time::Duration;

use crate::gui::Gui;
use crate::wayland_client::WaylandClient;
use crate::wayland_client::WaylandClientEvent;
use crate::wgpu_wrapper::WgpuWrapper;
use anyhow::bail;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, trace, warn};
use wayland_client::EventQueue;

#[derive(Debug)]
pub enum MaybeWgpuWrapper {
    Uninitialized,
    Initializing,
    Initialized(WgpuWrapper),
}

#[derive(Debug)]
enum DaemonEvent {
    WgpuInit(anyhow::Result<WgpuWrapper>),
    Show,
    Hide,
}

pub struct Daemon {
    height: u32,
    width: u32,
    wgpu: MaybeWgpuWrapper,
    wayland_client: WaylandClient,
    wayland_client_q: EventQueue<WaylandClient>,
    wayland_client_rx: UnboundedReceiver<WaylandClientEvent>,

    command_tx: UnboundedSender<DaemonEvent>,
    command_rx: UnboundedReceiver<DaemonEvent>,
    gui: Gui,
    requested_repaint: bool,
    visible: bool,
}

impl Daemon {
    pub async fn start() -> anyhow::Result<()> {
        let (wayland_client, wayland_client_q, wayland_client_rx) = WaylandClient::init()?;

        let (command_tx, command_rx) = mpsc::unbounded_channel();

        debug!("Initialized wayland layer client");

        let mut daemon = Self {
            height: 400,
            width: 800,
            wgpu: MaybeWgpuWrapper::Uninitialized,
            wayland_client,
            wayland_client_q,
            wayland_client_rx,
            command_tx,
            command_rx,
            gui: Default::default(),
            requested_repaint: false,
            visible: false,
        };

        Daemon::run_loop(&mut daemon).await
    }

    async fn run_loop(&mut self) -> anyhow::Result<()> {
        loop {
            self.wayland_client_q.flush()?;

            let Some(read_guard) = self.wayland_client_q.prepare_read() else {
                debug!("no read guard, events pending");
                self.wayland_client_q
                    .dispatch_pending(&mut self.wayland_client)?;
                continue;
            };

            let async_fd = AsyncFd::new(read_guard.connection_fd())?;

            tokio::select! {
                _ = async_fd.readable() => {
                    trace!("fd is readable, events pending");

                    drop(async_fd);

                    read_guard.read()?;

                    self.wayland_client_q
                        .dispatch_pending(&mut self.wayland_client)?;
                },
                Some(event) = self.wayland_client_rx.recv() => {
                    trace!("received wayland client event {:?}", event);

                    match event {
                        WaylandClientEvent::LayerShellConfigure(configure) => {
                            let (width, height) = configure.new_size;
                            match self.wgpu {
                                MaybeWgpuWrapper::Uninitialized => {
                                    self.wgpu = MaybeWgpuWrapper::Initializing;

                                    let command_tx = self.command_tx.clone();
                                    let raw_handles = self.wayland_client.get_raw_handles()?;

                                    tokio::spawn(async move {
                                        let wgpu_wrapper = WgpuWrapper::init(raw_handles, 800, 400).await;
                                        command_tx.send(DaemonEvent::WgpuInit(wgpu_wrapper)).unwrap();
                                    });
                                }
                                MaybeWgpuWrapper::Initializing => (), // posible edge case
                                MaybeWgpuWrapper::Initialized(_) => self.update_size(width, height)?
                            }
                        }
                        WaylandClientEvent::Egui(events) => {

                            for event in events.iter() {
                                match event {
                                    egui::Event::Key { key, .. } => tracing::info!("keyboard event: {:?}", key),
                                    egui::Event::PointerMoved { .. } => tracing::info!("pointer event"),
                                    _ => ()
                                }
                            }

                            self.gui.handle_events(events);


                            if self.gui.needs_repaint() {
                                self.request_repaint();
                            }
                        }
                        WaylandClientEvent::Frame => self.paint()?,
                        WaylandClientEvent::Hide => self.update_visibility(false)?
                    }
                },
                Some(event) = self.command_rx.recv() => {
                    trace!("received daemon event {:?}", event);

                    match event {
                        DaemonEvent::WgpuInit(wgpu_wrapper_result) =>
                            match wgpu_wrapper_result {
                                Ok(wgpu_wrapper) => {
                                    self.wgpu = MaybeWgpuWrapper::Initialized(wgpu_wrapper);
                                    let command_tx = self.command_tx.clone();
                                    tokio::spawn(async move {
                                        loop {
                                            thread::sleep(Duration::from_secs(3));
                                            command_tx.send(DaemonEvent::Show).unwrap();
                                            thread::sleep(Duration::from_secs(5));
                                            command_tx.send(DaemonEvent::Hide).unwrap();
                                        }
                                    });
                                }
                                Err(err) => bail!(err)
                            }
                        DaemonEvent::Show => self.update_visibility(true)?,
                        DaemonEvent::Hide => self.update_visibility(false)?
                    }
                }
            }
        }
    }

    fn request_repaint(&mut self) {
        if self.requested_repaint {
            return;
        }
        self.requested_repaint = true;

        trace!("repaint requested");
        self.wayland_client.wl_surface.frame(
            &self.wayland_client_q.handle(),
            self.wayland_client.wl_surface.clone(),
        );
        self.wayland_client.wl_surface.commit();
    }

    fn update_visibility(&mut self, visible: bool) -> anyhow::Result<()> {
        if self.visible == visible {
            return Ok(());
        }

        self.visible = visible;

        if visible {
            self.wayland_client
                .layer_surface
                .set_keyboard_interactivity(KeyboardInteractivity::Exclusive);

            // TODO: move sizing out of here
            self.wayland_client
                .layer_surface
                .set_size(self.width, self.height);

            self.wayland_client.layer_surface.commit();
        } else {
            self.wayland_client
                .layer_surface
                .set_keyboard_interactivity(KeyboardInteractivity::None);
        }

        self.paint()
    }

    fn paint(&mut self) -> anyhow::Result<()> {
        self.requested_repaint = false;

        if self.visible {
            if let MaybeWgpuWrapper::Initialized(wgpu) = &mut self.wgpu {
                return self.gui.paint(wgpu);
            }

            warn!("paint requested but wgpu has not yet finished initializing");
        }

        self.wayland_client.wl_surface.attach(None, 0, 0);
        self.wayland_client.wl_surface.commit();
        Ok(())
    }

    fn update_size(&mut self, width: u32, height: u32) -> anyhow::Result<()> {
        let MaybeWgpuWrapper::Initialized(wgpu) = &mut self.wgpu else {
            bail!("attempting to update size but wgpu is uninitialized!");
        };

        wgpu.update_size(width, height);
        Ok(())
    }
}
