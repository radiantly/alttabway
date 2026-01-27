use std::fmt::Debug;

use egui::{
    Align, ClippedPrimitive, Color32, Context, CursorIcon, Event, Frame, FullOutput, Image, Label,
    Layout, RawInput, Stroke, TexturesDelta, UiBuilder,
};

use crate::gui_state::GuiState;

pub struct Gui {
    egui_ctx: Context,

    state: GuiState,
    cursor_icon: CursorIcon,
}

impl Debug for Gui {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gui").finish()
    }
}

impl Default for Gui {
    fn default() -> Self {
        let context = Context::default();
        context.style_mut(|style| style.visuals.override_text_color = Some(Color32::WHITE));
        Self {
            egui_ctx: context,
            state: Default::default(),
            cursor_icon: CursorIcon::Default,
        }
    }
}

impl Gui {
    pub fn new() -> Self {
        Gui::default()
    }

    pub fn add_item(&mut self, id: u32) {
        self.state.add_item(id);
    }

    pub fn update_item_title(&mut self, id: u32, new_title: String) {
        self.state.update_item_title(id, new_title);
    }
    pub fn update_item_app_id(&mut self, id: u32, new_app_id: String) {
        self.state.update_item_app_id(id, new_app_id);
    }
    pub fn signal_item_activation(&mut self, id: u32) {
        self.state.signal_item_activation(id);
    }
    pub fn remove_item(&mut self, id: u32) {
        self.state.remove_item(id);
    }
    pub fn get_first_item_id(&self) -> Option<u32> {
        self.state.get_first_item_id()
    }
    pub fn update_item_preview(&mut self, id: u32, preview_rgba: &[u8], preview_width: u32) {
        self.state.update_item_preview(
            id,
            (preview_rgba, preview_width as usize),
            |name, color_image| {
                self.egui_ctx
                    .load_texture(name, color_image, Default::default())
            },
        );
    }

    pub fn reset_selected_item(&mut self) {
        self.state.reset_selected_item();
    }

    pub fn get_selected_item_id(&self) -> Option<u32> {
        self.state.get_selected_item_id()
    }

    pub fn calculate_preview_size(&self, current_size: (u32, u32)) -> (u32, u32) {
        self.state.calculate_preview_size(current_size)
    }

    pub fn select_previous_item(&mut self) {
        self.state.select_previous_item()
    }

    pub fn select_next_item(&mut self) {
        self.state.select_next_item()
    }

    pub fn handle_events(&mut self, mut events: Vec<Event>) {
        for event in &mut events {
            if let Event::Key {
                key: egui::Key::Tab,
                pressed: true,
                modifiers,
                ..
            } = event
            {
                match modifiers.shift {
                    true => self.state.select_previous_item(),
                    false => self.state.select_next_item(),
                }
            }
        }

        let raw_input = RawInput {
            events,
            focused: true,
            ..Default::default()
        };
        self.build_ui(raw_input);
    }

    pub fn get_window_dimensions(&mut self) -> (u32, u32) {
        let layout = self.state.calculate_layout();
        (layout.computed.window_width, layout.computed.window_height)
    }

    fn build_ui(&mut self, raw_input: RawInput) -> FullOutput {
        let layout = self.state.calculate_layout();
        let mut hovered_item_updated = None;

        let full_output = self.egui_ctx.run(raw_input, |ctx: &Context| {
            let panel_frame = egui::Frame::new()
                .fill(layout.params.window_background)
                .corner_radius(layout.params.window_corner_radius);

            egui::CentralPanel::default()
                .frame(panel_frame)
                .show(ctx, |ui| {
                    for (index, (rect, item)) in layout
                        .computed
                        .item_rects
                        .iter()
                        .zip(layout.items)
                        .enumerate()
                    {
                        let mut frame_ui = ui.new_child(UiBuilder::new().max_rect(*rect));

                        let mut frame = Frame::default()
                            .stroke(Stroke::new(
                                layout.params.item_stroke as f32,
                                Color32::TRANSPARENT,
                            ))
                            .inner_margin(layout.params.item_padding as f32)
                            .corner_radius(layout.params.item_corner_radius)
                            .begin(&mut frame_ui);
                        {
                            let ui = &mut frame.content_ui;
                            ui.allocate_ui_with_layout(
                                (ui.available_width(), layout.params.title_height as f32).into(),
                                Layout::left_to_right(Align::Center),
                                |ui| ui.add(Label::new(item.get_title()).truncate()),
                            );
                            if let Some((handle, [width, height])) = item.get_preview() {
                                ui.add(
                                    Image::from_texture((
                                        handle.id(),
                                        (*width as f32, *height as f32).into(),
                                    ))
                                    .corner_radius(layout.params.preview_corner_radius),
                                );
                            } else {
                                ui.allocate_space(ui.available_size());
                            }
                        }

                        let response = frame.allocate_space(&mut frame_ui);
                        if response.hovered() {
                            hovered_item_updated = index.into();
                        }

                        if layout.selected_item == index {
                            frame.frame.stroke.color = Color32::WHITE;
                            frame.frame.fill = layout.params.item_active_background;
                        } else if let Some(hovered_item) = layout.hovered_item
                            && hovered_item == index
                        {
                            frame.frame.fill = layout.params.item_hover_background;
                        }
                        frame.paint(&frame_ui);
                    }
                });
        });

        self.cursor_icon = match hovered_item_updated {
            Some(_) => CursorIcon::PointingHand,
            None => CursorIcon::Default,
        };

        self.state.set_hovered_item(hovered_item_updated);

        full_output
    }

    pub fn needs_repaint(&self) -> bool {
        self.state.needs_repaint()
    }

    pub fn get_cursor_icon(&mut self) -> &CursorIcon {
        &self.cursor_icon
    }

    pub fn get_output(
        &mut self,
        width: f32,
        height: f32,
    ) -> (TexturesDelta, Vec<ClippedPrimitive>) {
        // Build egui UI with collected events
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )),
            focused: true,
            ..Default::default()
        };

        let full_output = self.build_ui(raw_input);
        let primitives = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        self.state.mark_repainted();
        (full_output.textures_delta, primitives)
    }
}
