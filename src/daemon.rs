use std::{thread, time::Duration};

use anyhow::bail;
use smithay_client_toolkit::{
    reexports::client::EventQueue,
    shell::{WaylandSurface, wlr_layer::KeyboardInteractivity},
};
use tokio::{
    io::unix::AsyncFd,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};
use tracing::{debug, trace, warn};

use crate::{
    gui::Gui,
    wayland_client::{WaylandClient, WaylandClientEvent},
    wgpu_wrapper::WgpuWrapper,
};

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

#[derive(Debug)]
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
    pending_repaint: bool,
    visible: bool,
    wl_buffer_attached: bool,
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
            pending_repaint: false,
            visible: false,
            wl_buffer_attached: false,
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
                Some(event) = self.wayland_client_rx.recv() => {
                    trace!("received wayland client event {:?}", event);

                    match event {
                        WaylandClientEvent::LayerShellConfigure(configure) => {
                            // TODO: the compositor can send us (0, 0) indicating that we are
                            // free to pick any size. Handle this case.
                            let (width, height) = configure.new_size;

                            match &mut self.wgpu {
                                MaybeWgpuWrapper::Uninitialized => {
                                    self.wgpu = MaybeWgpuWrapper::Initializing;

                                    let command_tx = self.command_tx.clone();
                                    let raw_handles = self.wayland_client.get_raw_handles()?;

                                    tokio::spawn(async move {
                                        let wgpu_wrapper = WgpuWrapper::init(raw_handles, 800, 400).await;
                                        command_tx.send(DaemonEvent::WgpuInit(wgpu_wrapper)).unwrap();
                                    });
                                }
                                MaybeWgpuWrapper::Initializing => warn!("configure called during wgpu initialization!"),
                                MaybeWgpuWrapper::Initialized(wgpu) => {
                                    assert!(width != 0 && height != 0);

                                    wgpu.update_size(width, height);

                                    // Important note.
                                    // If at any point wl_surface.commit() is called without an attached buffer,
                                    // the compositor may just send a configure event
                                    // may result in an infinite loop if not careful

                                    if !self.visible {
                                        continue;
                                    }

                                    self.request_repaint()?
                                }
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
                    }
                },
                Some(event) = self.command_rx.recv() => {
                    trace!("received daemon event {:?}", event);

                    match event {
                        DaemonEvent::WgpuInit(wgpu_wrapper_result) =>
                            match wgpu_wrapper_result {
                                Ok(wgpu_wrapper) => {
                                    self.wgpu = MaybeWgpuWrapper::Initialized(wgpu_wrapper);
                                    self.request_repaint()?;

                                    // TODO: for debugging, to be removed
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

    fn request_repaint(&mut self) -> anyhow::Result<()> {
        if !self.wl_buffer_attached {
            return self.paint();
        }

        if self.pending_repaint {
            return Ok(());
        }
        self.pending_repaint = true;

        trace!("repaint requested");
        self.wayland_client.wl_surface.frame(
            &self.wayland_client_q.handle(),
            self.wayland_client.wl_surface.clone(),
        );
        self.wayland_client.wl_surface.commit();
        Ok(())
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
        } else {
            self.wayland_client
                .layer_surface
                .set_keyboard_interactivity(KeyboardInteractivity::None);
        }

        self.wayland_client.layer_surface.commit();

        self.request_repaint()
    }

    fn paint(&mut self) -> anyhow::Result<()> {
        self.pending_repaint = false;

        if self.visible {
            if let MaybeWgpuWrapper::Initialized(wgpu) = &mut self.wgpu {
                self.wl_buffer_attached = true;
                return self.gui.paint(wgpu);
            }

            warn!("paint requested but wgpu has not yet finished initializing");
        }

        if self.wl_buffer_attached {
            self.wl_buffer_attached = false;
            self.wayland_client.wl_surface.attach(None, 0, 0);
            self.wayland_client.wl_surface.commit();
        }
        Ok(())
    }
}
