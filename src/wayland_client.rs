use std::ffi::c_void;
use std::ptr::NonNull;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use anyhow::Context;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::delegate_compositor;
use smithay_client_toolkit::delegate_layer;
use smithay_client_toolkit::delegate_output;
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle};

pub enum WaylandClientEvent {
    LayerShellConfigure(LayerSurfaceConfigure),
}

#[derive(Debug)]
pub struct WaylandClient {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor_state: CompositorState,
    layer_shell: LayerShell,
    pub layer_surface: LayerSurface,
    pub wl_surface: WlSurface,
    connection: Connection,
    wl_tx: UnboundedSender<WaylandClientEvent>,
}

pub struct RawHandles {
    pub raw_display_handle: RawDisplayHandle,
    pub raw_window_handle: RawWindowHandle,
}

unsafe impl Send for RawHandles {}

impl WaylandClient {
    pub fn init() -> anyhow::Result<(
        Self,
        EventQueue<Self>,
        UnboundedReceiver<WaylandClientEvent>,
    )> {
        let connection = Connection::connect_to_env()?;
        let (globals, event_queue): (_, EventQueue<Self>) = registry_queue_init(&connection)?;
        let qh = event_queue.handle();
        let compositor_state = CompositorState::bind(&globals, &qh)?;
        let wl_surface = compositor_state.create_surface(&qh);
        let layer_shell = LayerShell::bind(&globals, &qh)?;
        let layer_surface = layer_shell.create_layer_surface(
            &qh,
            wl_surface.clone(),
            Layer::Overlay,
            Some(env!("CARGO_CRATE_NAME")),
            None,
        );

        // Anchor to top and horizontally centered
        layer_surface.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT | Anchor::BOTTOM);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.set_exclusive_zone(-1); // Don't reserve space
        layer_surface.set_size(0, 0);
        layer_surface.set_margin(0, 0, 0, 0);
        layer_surface.commit();

        let (wl_tx, wl_rx) = mpsc::unbounded_channel();

        let wayland_app = Self {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            connection,
            compositor_state,
            layer_shell,
            layer_surface,
            wl_surface,
            wl_tx,
        };

        Ok((wayland_app, event_queue, wl_rx))
    }

    pub fn get_raw_handles(&self) -> anyhow::Result<RawHandles> {
        let display_ptr = self.connection.backend().display_ptr() as *mut c_void;
        let surface_ptr = self.wl_surface.id().as_ptr() as *mut c_void;

        let raw_display_handle = {
            let display = NonNull::new(display_ptr).context("display_ptr is null")?;
            let handle = WaylandDisplayHandle::new(display);
            RawDisplayHandle::Wayland(handle)
        };

        let raw_window_handle = {
            let surface = NonNull::new(surface_ptr).context("surface_ptr is null")?;
            let handle = WaylandWindowHandle::new(surface);
            RawWindowHandle::Wayland(handle)
        };

        Ok(RawHandles {
            raw_display_handle,
            raw_window_handle,
        })
    }
}

impl CompositorHandler for WaylandClient {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new_transform: wayland_client::protocol::wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _time: u32,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }
}

impl OutputHandler for WaylandClient {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {}

    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {}

    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {
    }
}

impl LayerShellHandler for WaylandClient {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        // self.handler.closed();
    }

    fn configure(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        layer_surface_configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        tracing::warn!(
            "conf {:?}",
            _connection.backend().display_ptr() as *mut c_void
        );
        self.wl_tx
            .send(WaylandClientEvent::LayerShellConfigure(
                layer_surface_configure,
            ))
            .unwrap();
    }
}

impl ProvidesRegistryState for WaylandClient {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}

delegate_compositor!(WaylandClient);
delegate_output!(WaylandClient);
delegate_layer!(WaylandClient);
delegate_registry!(WaylandClient);
