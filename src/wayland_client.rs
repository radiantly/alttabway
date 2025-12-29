use std::ffi::c_void;
use std::ptr::NonNull;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use anyhow::Context;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::delegate_compositor;
use smithay_client_toolkit::delegate_keyboard;
use smithay_client_toolkit::delegate_layer;
use smithay_client_toolkit::delegate_output;
use smithay_client_toolkit::delegate_pointer;
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::delegate_seat;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle};

#[derive(Debug)]
pub enum WaylandClientEvent {
    LayerShellConfigure(LayerSurfaceConfigure),
    Egui(Vec<egui::Event>),
    Frame,
    Hide,
}

impl WaylandClientEvent {
    fn to_egui_modifier(modifiers: Modifiers) -> egui::Modifiers {
        egui::Modifiers {
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            mac_cmd: false,
            command: modifiers.ctrl,
        }
    }

    fn to_egui_button(button: u32) -> egui::PointerButton {
        match button {
            272 => egui::PointerButton::Primary,
            273 => egui::PointerButton::Secondary,
            274 => egui::PointerButton::Middle,
            _ => egui::PointerButton::Extra1,
        }
    }

    fn to_egui_pos2(position: (f64, f64)) -> egui::Pos2 {
        egui::Pos2 {
            x: position.0 as f32,
            y: position.1 as f32,
        }
    }
}

impl TryFrom<(&[PointerEvent], Modifiers)> for WaylandClientEvent {
    type Error = &'static str;

    fn try_from(value: (&[PointerEvent], Modifiers)) -> Result<Self, Self::Error> {
        let (pointer_events, modifiers) = value;
        let modifiers = Self::to_egui_modifier(modifiers);

        let events: Vec<_> = pointer_events
            .iter()
            .filter_map(|event| match event.kind {
                PointerEventKind::Motion { .. } => Some(egui::Event::PointerMoved(
                    Self::to_egui_pos2(event.position),
                )),
                PointerEventKind::Press { button, .. } => Some(egui::Event::PointerButton {
                    pos: Self::to_egui_pos2(event.position),
                    button: Self::to_egui_button(button),
                    pressed: true,
                    modifiers,
                }),
                PointerEventKind::Release { button, .. } => Some(egui::Event::PointerButton {
                    pos: Self::to_egui_pos2(event.position),
                    button: Self::to_egui_button(button),
                    pressed: false,
                    modifiers,
                }),
                _ => None,
            })
            .collect();

        if events.is_empty() {
            return Err("no relevant pointer events to send!");
        }
        Ok(Self::Egui(events))
    }
}

impl TryFrom<(KeyEvent, bool, bool, Modifiers)> for WaylandClientEvent {
    type Error = &'static str;

    fn try_from(value: (KeyEvent, bool, bool, Modifiers)) -> Result<Self, Self::Error> {
        let (key_event, pressed, repeat, modifiers) = value;
        let modifiers = Self::to_egui_modifier(modifiers);

        if let Keysym::Escape = key_event.keysym {
            return Ok(WaylandClientEvent::Hide);
        }

        let key = match key_event.keysym {
            Keysym::Up => egui::Key::ArrowUp,
            Keysym::Down => egui::Key::ArrowDown,
            Keysym::Left => egui::Key::ArrowLeft,
            Keysym::Right => egui::Key::ArrowRight,
            Keysym::Tab => egui::Key::Tab,
            Keysym::Return => egui::Key::Enter,
            _ => return Err("keyboard event not mapped"),
        };

        let event = egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers,
        };

        Ok(Self::Egui(vec![event]))
    }
}

#[derive(Debug)]
pub struct WaylandClient {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor_state: CompositorState,
    layer_shell: LayerShell,
    seat_state: SeatState,
    pub layer_surface: LayerSurface,
    pub wl_surface: WlSurface,
    connection: Connection,
    wl_tx: UnboundedSender<WaylandClientEvent>,
    modifiers: Modifiers,
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

        let seat_state = SeatState::new(&globals, &qh);

        let wayland_app = Self {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            connection,
            compositor_state,
            layer_shell,
            seat_state,
            layer_surface,
            wl_surface,
            wl_tx,
            modifiers: Default::default(),
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
        self.wl_tx.send(WaylandClientEvent::Frame).unwrap();
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

    registry_handlers![OutputState, SeatState];
}

impl SeatHandler for WaylandClient {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && self.seat_state.get_keyboard(qh, &seat, None).is_err()
        {
            tracing::warn!("Failed to get keyboard capability");
        }

        if capability == Capability::Pointer && self.seat_state.get_pointer(qh, &seat).is_err() {
            tracing::warn!("Failed to get pointer capability");
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wayland_client::protocol::wl_seat::WlSeat,
        _capability: Capability,
    ) {
    }

    fn remove_seat(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wayland_client::protocol::wl_seat::WlSeat,
    ) {
    }
}

impl KeyboardHandler for WaylandClient {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wayland_client::protocol::wl_keyboard::WlKeyboard,
        _surface: &WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wayland_client::protocol::wl_keyboard::WlKeyboard,
        _surface: &WlSurface,
        _serial: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wayland_client::protocol::wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        if let Ok(event) = (event, true, false, self.modifiers).try_into() {
            self.wl_tx.send(event).unwrap()
        }
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wayland_client::protocol::wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        if let Ok(event) = (event, false, false, self.modifiers).try_into() {
            self.wl_tx.send(event).unwrap()
        }
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wayland_client::protocol::wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw_modifiers: smithay_client_toolkit::seat::keyboard::RawModifiers,
        _layout: u32,
    ) {
        self.modifiers = modifiers;
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wayland_client::protocol::wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        if let Ok(event) = (event, true, true, self.modifiers).try_into() {
            self.wl_tx.send(event).unwrap()
        }
    }
}

impl PointerHandler for WaylandClient {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wayland_client::protocol::wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        if let Ok(event) = (events, self.modifiers).try_into() {
            self.wl_tx.send(event).unwrap()
        }
    }
}

delegate_compositor!(WaylandClient);
delegate_output!(WaylandClient);
delegate_layer!(WaylandClient);
delegate_seat!(WaylandClient);
delegate_keyboard!(WaylandClient);
delegate_pointer!(WaylandClient);
delegate_registry!(WaylandClient);
