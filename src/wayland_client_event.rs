use anyhow::bail;
use smithay_client_toolkit::{
    seat::{
        keyboard::{KeyEvent, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind},
    },
    shell::wlr_layer::LayerSurfaceConfigure,
    shm::slot::Buffer,
};

#[derive(Debug)]
pub enum WaylandClientEvent {
    LayerShellConfigure(LayerSurfaceConfigure),
    Egui(Vec<egui::Event>),
    ModifierChange,
    PaintRequest,
    TopLevelAdded(u32),
    TopLevelActivated(u32),
    TopLevelTitleUpdate(u32, String),
    TopLevelAppIdUpdate(u32, String),
    TopLevelRemoved(u32),
    ScreencopyDone(u32, Buffer),
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

    pub fn from_wl_pointer_events(
        pointer_events: &[PointerEvent],
        modifiers: Modifiers,
    ) -> anyhow::Result<Self> {
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
            bail!("no relevant pointer events to send!");
        }
        Ok(Self::Egui(events))
    }

    pub fn from_wl_key_event(
        key_event: KeyEvent,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
    ) -> anyhow::Result<Self> {
        let modifiers = Self::to_egui_modifier(modifiers);

        let key = match key_event.keysym {
            Keysym::Up => egui::Key::ArrowUp,
            Keysym::Down => egui::Key::ArrowDown,
            Keysym::Left => egui::Key::ArrowLeft,
            Keysym::Right => egui::Key::ArrowRight,
            Keysym::Tab | Keysym::ISO_Left_Tab => egui::Key::Tab,
            Keysym::Return => egui::Key::Enter,
            _ => bail!("keyboard event not mapped"),
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
