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
            self, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
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
        pointer::{
            CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
        },
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
use tracing::{debug, warn};

use crate::wayland_client_event::WaylandClientEvent;

#[derive(Default, Debug)]
pub struct ScreencopyFrameState {
    id: u32,
    width: i32,
    height: i32,
    stride: i32,
    format: Option<Format>,
    buffer: Option<Buffer>,
}

impl ScreencopyFrameState {
    fn new(id: u32) -> Self {
        Self {
            id,
            ..Default::default()
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
    surfaces: Option<Surfaces>,
    wl_tx: UnboundedSender<WaylandClientEvent>,
    modifiers: Modifiers,
    toplevel_windows: Vec<ZwlrForeignToplevelHandleV1>,
    pool: SlotPool,

    screencopy_manager: ZwlrScreencopyManagerV1,
    screencopy_frames: HashMap<ZwlrScreencopyFrameV1, ScreencopyFrameState>,

    themed_pointer: Option<ThemedPointer>,
    current_cursor: Option<CursorIcon>,
    requested_cursor: CursorIcon,
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

        // TODO: dynamic resizing
        let pool = SlotPool::new(1920 * 1920 * 4, &shm)?;

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
            pool,

            themed_pointer: None,
            current_cursor: None,
            requested_cursor: CursorIcon::Default,
        };

        Ok((wayland_app, event_queue, wl_rx))
    }

    pub fn get_modifiers(&self) -> &Modifiers {
        &self.modifiers
    }

    fn set_cursor(&mut self) {
        if let Some(ref mut pointer) = self.themed_pointer {
            if pointer
                .set_cursor(&self.connection, self.requested_cursor)
                .is_ok()
            {
                self.current_cursor = self.requested_cursor.into();
            }
        }
    }

    pub fn request_cursor(&mut self, icon: CursorIcon) {
        self.requested_cursor = icon;
        if let Some(current_icon) = self.current_cursor
            && current_icon != icon
        {
            self.set_cursor();
        }
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
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
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

    pub fn create_buffer(&mut self, width: i32, height: i32) -> anyhow::Result<Buffer> {
        let (buffer, _) = self
            .pool
            .create_buffer(width, height, width * 4, Format::Argb8888)?;
        Ok(buffer)
    }

    pub fn get_buffer_mut<T>(
        &mut self,
        buffer: &Buffer,
        handle_buffer: impl FnOnce(&mut [u8]) -> T,
    ) -> T {
        let canvas = buffer.canvas(&mut self.pool).unwrap();

        handle_buffer(canvas)
    }

    pub fn update_surface_buffer(&mut self, buffer: &Buffer) {
        let Some(Surfaces { wl_surface, .. }) = &mut self.surfaces else {
            tracing::warn!("No active surface");
            return;
        };

        wl_surface.attach(Some(buffer.wl_buffer()), 0, 0);
        wl_surface.damage_buffer(0, 0, buffer.stride() / 4, buffer.height());
        wl_surface.commit();
    }

    pub fn has_surfaces(&self) -> bool {
        self.surfaces.is_some()
    }

    pub fn destroy_surfaces(&mut self) {
        self.current_cursor = None;
        self.surfaces.take();
    }

    pub fn request_paint(&mut self, qh: &QueueHandle<Self>) {
        if let Some(surfaces) = &self.surfaces {
            surfaces.wl_surface.frame(qh, surfaces.wl_surface.clone());
            surfaces.wl_surface.commit();
        }
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

    pub fn activate_window(&mut self, id: u32) {
        let Some(window_handle) = self
            .toplevel_windows
            .iter()
            .find(|window| window.id().protocol_id() == id)
        else {
            return;
        };

        let seat_count = self.seat_state.seats().count();
        tracing::debug!("activating window {}, seat count {}", id, seat_count);

        if let Some(seat) = &self.seat_state.seats().next() {
            window_handle.activate(seat);
        };
    }

    pub fn capture_window_region(
        &mut self,
        id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        queue_handle: &QueueHandle<Self>,
    ) -> anyhow::Result<()> {
        if width <= 0 || height <= 0 {
            return Ok(());
        }

        // I think this function can be much nicer if we get window preview via
        // ext_foreign_toplevel_image_capture_source_manager_v1

        // TODO: stitch windows that are part of different monitors

        // Find the output that contains the given coordinates based on logical_position
        let output_with_coords = self.output_state.outputs().find_map(|output| {
            let Some(info) = self.output_state.info(&output) else {
                return None;
            };
            let (output_x, output_y) = info.logical_position.unwrap_or(info.location);
            let (output_w, output_h) = info.logical_size.unwrap_or_default();

            let relative_x = x - output_x;
            let relative_y = y - output_y;

            if (0..=output_w).contains(&relative_x) && (0..=output_h).contains(&relative_y) {
                (output, relative_x, relative_y).into()
            } else {
                None
            }
        });

        let Some((output, relative_x, relative_y)) = output_with_coords else {
            debug!("no output contains coordinates ({}, {})", x, y);
            return Ok(());
        };

        let frame = self.screencopy_manager.capture_output_region(
            0,
            &output,
            relative_x,
            relative_y,
            width,
            height,
            queue_handle,
            (),
        );

        self.screencopy_frames
            .insert(frame, ScreencopyFrameState::new(id));

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
        self.wl_tx.send(WaylandClientEvent::PaintRequest).unwrap();
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

        if capability == Capability::Pointer && self.themed_pointer.is_none() {
            let surface = self.compositor_state.create_surface(qh);
            match self.seat_state.get_pointer_with_theme(
                qh,
                &seat,
                self.shm.wl_shm(),
                surface,
                ThemeSpec::default(),
            ) {
                Ok(pointer) => self.themed_pointer = Some(pointer),
                Err(e) => tracing::warn!("Failed to get themed pointer: {:?}", e),
            }
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
        if let Ok(event) = WaylandClientEvent::from_wl_key_event(event, true, false, self.modifiers)
        {
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
        if let Ok(event) =
            WaylandClientEvent::from_wl_key_event(event, false, false, self.modifiers)
        {
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
        self.wl_tx.send(WaylandClientEvent::ModifierChange).unwrap();
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        if let Ok(event) = WaylandClientEvent::from_wl_key_event(event, true, true, self.modifiers)
        {
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
        // Set cursor on enter events
        for event in events {
            if matches!(event.kind, PointerEventKind::Enter { .. }) {
                self.set_cursor();
                break;
            }
        }

        if let Ok(event) = WaylandClientEvent::from_wl_pointer_events(events, self.modifiers) {
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

                tracing::debug!("widthxheight (stride) {}x{} ({})", width, height, stride);

                let Ok((buffer, _)) = state.pool.create_buffer(width, height, stride, format)
                else {
                    tracing::error!("could not create buffer from pool!");
                    state.screencopy_frames.remove(frame);
                    return;
                };

                frame.copy(buffer.wl_buffer());
                frame_state.buffer = buffer.into();
            }
            Event::Flags { flags } => {
                use zwlr_screencopy_frame_v1::Flags;

                match flags {
                    WEnum::Value(flags) => {
                        if flags.contains(Flags::YInvert) {
                            warn!("TODO: Handle screencopy YInvert");
                        }
                    }
                    WEnum::Unknown(flag) => warn!("Unknown screencopy flag: {}", flag),
                };
            }
            Event::Ready { .. } => {
                tracing::debug!(
                    "ready buffer {:?}",
                    &frame_state
                        .buffer
                        .as_ref()
                        .unwrap()
                        .canvas(&mut state.pool)
                        .context("missing")
                        .unwrap()[0..4]
                );

                if let Some(ScreencopyFrameState {
                    id,
                    buffer: Some(buffer),
                    ..
                }) = state.screencopy_frames.remove(frame)
                {
                    state
                        .wl_tx
                        .send(WaylandClientEvent::ScreencopyDone(id, buffer))
                        .unwrap();
                }
                frame.destroy();
            }
            Event::Failed => {
                tracing::warn!("Screencopy failed");
                state.screencopy_frames.remove(frame);
            }

            // currently unused because we never call frame.copy_with_damage
            Event::Damage { .. } => {}

            // LinuxDmabuf is a possible perf enhancement that can be explored in the future
            Event::LinuxDmabuf { .. } => {}
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
                state
                    .wl_tx
                    .send(WaylandClientEvent::TopLevelAdded(
                        toplevel.id().protocol_id(),
                    ))
                    .unwrap();
                state.toplevel_windows.push(toplevel);
            }
            Event::Finished => {
                state.toplevel_windows.clear();
            }
            _ => unimplemented!(),
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

        let id = handle.id().protocol_id();

        let client_event = match event {
            Event::Title { title } => WaylandClientEvent::TopLevelTitleUpdate(id, title).into(),
            Event::AppId { app_id } => WaylandClientEvent::TopLevelAppIdUpdate(id, app_id).into(),
            Event::Closed => WaylandClientEvent::TopLevelRemoved(id).into(),
            Event::State {
                state: window_state,
            } => {
                let activated = zwlr_foreign_toplevel_handle_v1::State::Activated as u8;

                if window_state.contains(&activated) {
                    WaylandClientEvent::TopLevelActivated(id).into()
                } else {
                    None
                }
            }

            // this set of events have all been processed
            Event::Done => None,

            // windows have changed monitors
            Event::OutputEnter { .. } => None,
            Event::OutputLeave { .. } => None,

            // when the parent of the toplevel changes(?)
            Event::Parent { .. } => None,
            _ => unimplemented!(),
        };

        let Some(client_event) = client_event else {
            return;
        };

        state.wl_tx.send(client_event).unwrap();
    }
}
