use anyhow::Context;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        client::{
            self, Connection, Dispatch, EventQueue, Proxy, QueueHandle,
            globals::registry_queue_init,
            protocol::{
                wl_keyboard::WlKeyboard,
                wl_output::{Transform, WlOutput},
                wl_pointer::WlPointer,
                wl_seat::WlSeat,
                wl_shm::Format,
                wl_surface::WlSurface,
            },
        },
        protocols_wlr::{
            foreign_toplevel::v1::client::{
                zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
                zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
            },
            screencopy::v1::client::{
                zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
                zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
            },
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use std::{collections::HashMap, ffi::c_void, ptr::NonNull};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::debug;
use wayland_backend::protocol::WEnum;

#[derive(Debug)]
pub enum WaylandClientEvent {
    LayerShellConfigure(LayerSurfaceConfigure),
    Egui(Vec<egui::Event>),
    Frame,
    Hide,
    Activate,
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
pub struct ScreencopyFrameState {
    window: ZwlrForeignToplevelHandleV1,

    width: i32,
    height: i32,
    stride: i32,
    format: Option<Format>,
    buffer: Option<Buffer>,
}

impl ScreencopyFrameState {
    fn new(window: ZwlrForeignToplevelHandleV1) -> Self {
        Self {
            window,

            width: Default::default(),
            height: Default::default(),
            stride: Default::default(),
            buffer: Default::default(),
            format: Default::default(),
        }
    }

    fn set_buffer_details(
        &mut self,
        width: u32,
        height: u32,
        stride: u32,
        format: impl Into<Format>,
    ) -> anyhow::Result<()> {
        self.width = width.try_into()?;
        self.height = height.try_into()?;
        self.stride = stride.try_into()?;
        self.format = format.into().into();
        Ok(())
    }

    fn get_buffer_details(&self) -> Option<(i32, i32, i32, Format)> {
        if let Some(format) = self.format
            && self.width > 0
            && self.height > 0
        {
            (self.width, self.height, self.stride, format).into()
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub struct PreviewImage {
    pub buffer: Buffer,
    pub is_rgba: bool,
}

#[derive(Debug)]
pub struct TopLevelWindow {
    handle: ZwlrForeignToplevelHandleV1,
    outputs: Vec<WlOutput>,
    pub title: String,
    pub app_id: String,
    pub preview: Option<PreviewImage>,
}

#[derive(Debug)]
pub struct Surfaces {
    pub layer_surface: LayerSurface,
    pub wl_surface: WlSurface,
}

#[derive(Debug)]
pub struct WaylandClient {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor_state: CompositorState,
    layer_shell: LayerShell,
    seat_state: SeatState,
    shm: Shm,
    connection: Connection,
    pub surfaces: Option<Surfaces>,
    wl_tx: UnboundedSender<WaylandClientEvent>,
    modifiers: Modifiers,
    pub toplevel_windows: Vec<TopLevelWindow>,

    screencopy_manager: ZwlrScreencopyManagerV1,
    screencopy_frames: HashMap<ZwlrScreencopyFrameV1, ScreencopyFrameState>,
    pub screenshot_pool: SlotPool,
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
        let layer_shell = LayerShell::bind(&globals, &qh)?;

        let (wl_tx, wl_rx) = mpsc::unbounded_channel();

        let seat_state = SeatState::new(&globals, &qh);

        // Bind the foreign toplevel manager
        let _toplevel_manager =
            globals.bind::<ZwlrForeignToplevelManagerV1, _, _>(&qh, 3..=3, ())?;

        // Bind screencopy manager
        let screencopy_manager = globals.bind::<ZwlrScreencopyManagerV1, _, _>(&qh, 1..=3, ())?;

        // Bind shared memory
        let shm = Shm::bind(&globals, &qh)?;
        let screenshot_pool = SlotPool::new(1920 * 1920 * 4, &shm)?;

        let wayland_app = Self {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            connection,
            compositor_state,
            layer_shell,
            seat_state,
            shm,
            surfaces: None,
            wl_tx,
            modifiers: Default::default(),
            toplevel_windows: Vec::new(),
            screencopy_manager,
            screencopy_frames: HashMap::new(),
            screenshot_pool,
        };

        Ok((wayland_app, event_queue, wl_rx))
    }

    pub fn create_surfaces(
        &mut self,
        queue_handle: &QueueHandle<Self>,
        width: u32,
        height: u32,
    ) -> anyhow::Result<()> {
        let wl_surface = self.compositor_state.create_surface(queue_handle);

        let layer_surface = self.layer_shell.create_layer_surface(
            queue_handle,
            wl_surface.clone(),
            Layer::Overlay,
            Some(env!("CARGO_CRATE_NAME")),
            None,
        );

        // Anchor to top and horizontally centered
        layer_surface.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT | Anchor::BOTTOM);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.set_exclusive_zone(-1); // Don't reserve space
        layer_surface.set_size(width, height);
        layer_surface.set_margin(0, 0, 0, 0);
        layer_surface.commit();

        let surfaces = Surfaces {
            wl_surface,
            layer_surface,
        };

        self.surfaces = Some(surfaces);

        Ok(())
    }

    pub fn get_raw_handles(&self) -> anyhow::Result<RawHandles> {
        let surfaces = self.surfaces.as_ref().context("surfaces is None")?;
        let display_ptr = self.connection.backend().display_ptr() as *mut c_void;
        let surface_ptr = surfaces.wl_surface.id().as_ptr() as *mut c_void;

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

    fn with_window(
        &mut self,
        handle: &ZwlrForeignToplevelHandleV1,
        f: impl FnOnce(&mut TopLevelWindow),
    ) {
        let Some(toplevel_window) = self
            .toplevel_windows
            .iter_mut()
            .find(|window| &window.handle == handle)
        else {
            return;
        };

        f(toplevel_window)
    }

    pub fn get_toplevel_windows(&mut self) -> &mut [TopLevelWindow] {
        &mut self.toplevel_windows
    }

    pub fn capture_active_window_region(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        queue_handle: &QueueHandle<Self>,
    ) -> anyhow::Result<()> {
        let Some(active_window) = self.toplevel_windows.first_mut() else {
            debug!("no active winwdow!");
            return Ok(());
        };

        let Some(output) = active_window.outputs.first() else {
            debug!("this window isn't visible????");
            return Ok(());
        };

        let frame = self.screencopy_manager.capture_output_region(
            0, // overlay_cursor: 1 means include cursor, 0 means don't include
            output,
            x,
            y,
            width,
            height,
            queue_handle,
            (),
        );

        self.screencopy_frames.insert(
            frame,
            ScreencopyFrameState::new(active_window.handle.clone()),
        );

        Ok(())
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
        _new_transform: Transform,
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
        _seat: WlSeat,
        _capability: Capability,
    ) {
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}
}

impl KeyboardHandler for WaylandClient {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
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
        _keyboard: &WlKeyboard,
        _surface: &WlSurface,
        _serial: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
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
        _keyboard: &WlKeyboard,
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
        _keyboard: &WlKeyboard,
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
        _keyboard: &WlKeyboard,
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
        _pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        if let Ok(event) = (events, self.modifiers).try_into() {
            self.wl_tx.send(event).unwrap()
        }
    }
}

impl ShmHandler for WaylandClient {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(WaylandClient);
delegate_output!(WaylandClient);
delegate_layer!(WaylandClient);
delegate_seat!(WaylandClient);
delegate_keyboard!(WaylandClient);
delegate_pointer!(WaylandClient);
delegate_registry!(WaylandClient);
delegate_shm!(WaylandClient);

// Screencopy manager implementation
impl Dispatch<ZwlrScreencopyManagerV1, ()> for WaylandClient {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrScreencopyManagerV1,
        _event: <ZwlrScreencopyManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

// Screencopy frame implementation
impl Dispatch<ZwlrScreencopyFrameV1, ()> for WaylandClient {
    fn event(
        state: &mut Self,
        frame: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event;

        let Some(frame_state) = state.screencopy_frames.get_mut(frame) else {
            return;
        };

        match event {
            Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                tracing::debug!(
                    "Screencopy buffer format: {:?}, size: {}x{}, stride: {}",
                    format,
                    width,
                    height,
                    stride
                );

                let WEnum::Value(format) = format else { return };

                match format {
                    Format::Argb8888 | Format::Xrgb8888 => (),
                    _ => return,
                };

                let _ = frame_state.set_buffer_details(width, height, stride, format);
            }
            Event::BufferDone => {
                let Some((width, height, stride, format)) = frame_state.get_buffer_details() else {
                    state.screencopy_frames.remove(frame);
                    return;
                };

                tracing::debug!("wsh {:?}:{:?}:{:?}", width, stride, height);

                let Ok((buffer, _)) = state
                    .screenshot_pool
                    .create_buffer(width, height, stride, format)
                else {
                    tracing::error!("could not create buffer from pool!");
                    state.screencopy_frames.remove(frame);
                    return;
                };

                frame.copy(buffer.wl_buffer());
                frame_state.buffer = buffer.into();
            }
            Event::Flags { flags: _ } => {
                tracing::warn!("TODO: Handle screencopy flags");
            }
            Event::Ready {
                tv_sec_hi: _,
                tv_sec_lo: _,
                tv_nsec: _,
            } => {
                tracing::debug!("Screencopy ready");
                if let Some(ScreencopyFrameState {
                    window,
                    buffer: Some(buffer),
                    ..
                }) = state.screencopy_frames.remove(frame)
                {
                    state.with_window(&window, |window| {
                        window.preview = PreviewImage {
                            buffer,
                            is_rgba: false,
                        }
                        .into()
                    });
                }
            }
            Event::Failed => {
                tracing::warn!("Screencopy failed");
                state.screencopy_frames.remove(frame);
            }
            Event::Damage {
                x: _,
                y: _,
                width: _,
                height: _,
            } => {}
            Event::LinuxDmabuf {
                format: _,
                width: _,
                height: _,
            } => {}
            _ => unimplemented!(),
        }
    }
}

// Foreign toplevel manager implementation
impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for WaylandClient {
    fn event(
        state: &mut Self,
        _manager: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_manager_v1::Event;

        match event {
            Event::Toplevel { toplevel } => {
                state.toplevel_windows.push(TopLevelWindow {
                    handle: toplevel,
                    title: String::new(),
                    app_id: String::new(),
                    outputs: Default::default(),
                    preview: None,
                });
            }
            Event::Finished => {
                tracing::info!(
                    "Toplevel manager finished - compositor is shutting down the protocol"
                );
                state.toplevel_windows.clear();
            }
            _ => {}
        }
    }

    client::event_created_child!(WaylandClient, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ())
    ]);
}

// Foreign toplevel handle implementation
impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for WaylandClient {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;

        match event {
            Event::Title { title } => {
                state.with_window(handle, |window| window.title = title);
            }
            Event::AppId { app_id } => {
                state.with_window(handle, |window| window.app_id = app_id);
            }
            Event::Closed => {
                state
                    .toplevel_windows
                    .retain(|window| &window.handle != handle);
            }
            Event::State {
                state: window_state,
            } => {
                let activated = zwlr_foreign_toplevel_handle_v1::State::Activated as u8;

                if window_state.contains(&activated) {
                    // rotate_right may be more efficient here
                    if let Some(pos) = state
                        .toplevel_windows
                        .iter()
                        .position(|w| w.handle == *handle)
                    {
                        let window = state.toplevel_windows.remove(pos);
                        state.toplevel_windows.insert(0, window);
                        let _ = state.wl_tx.send(WaylandClientEvent::Activate);
                    }
                }
            }

            // this set of events have all been processed
            Event::Done => {
                let list = state
                    .toplevel_windows
                    .iter()
                    .map(|window| window.app_id.clone())
                    .collect::<Vec<_>>()
                    .join(", ");

                tracing::trace!("windows: {}", list);
            }

            // windows have changed monitors
            Event::OutputEnter { output } => {
                state.with_window(handle, |window| window.outputs.push(output));
            }
            Event::OutputLeave { output } => {
                state.with_window(handle, |window| window.outputs.retain(|o| o != &output));
            }

            // when the parent of the toplevel changes(?)
            Event::Parent { parent: _ } => (),
            _ => todo!(),
        }
    }
}
