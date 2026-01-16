use std::{cmp, fmt::Debug, iter, time::Duration};

use egui::{ColorImage, Context, Event, FullOutput, RawInput, TextureHandle, ViewportId};

use crate::wgpu_wrapper::{WgpuSurface, WgpuWrapper};

#[derive(Default)]
pub struct Item {
    id: u32,
    title: String,
    app_id: String,
    preview: Option<TextureHandle>,
}

impl Item {
    fn new(id: u32) -> Self {
        Self {
            id,
            ..Default::default()
        }
    }
}

trait ItemVecExt {
    fn with_id(&mut self, id: u32, f: impl FnOnce(&mut Item));
}

impl ItemVecExt for Vec<Item> {
    fn with_id(&mut self, id: u32, f: impl FnOnce(&mut Item)) {
        let Some(item) = self.iter_mut().find(|item| item.id == id) else {
            return;
        };
        f(item).into()
    }
}

pub struct Gui {
    egui_ctx: Context,
    egui_renderer: Option<egui_wgpu::Renderer>,
    needs_repaint: bool,

    items: Vec<Item>,
    selected_item: usize,
}

impl Debug for Gui {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gui")
            .field("needs_repaint", &self.needs_repaint)
            .finish()
    }
}

impl Default for Gui {
    fn default() -> Self {
        Self {
            items: vec![],
            egui_ctx: Context::default(),
            egui_renderer: None,
            needs_repaint: true,
            selected_item: 0,
        }
    }
}

impl Gui {
    pub fn new() -> Self {
        Gui::default()
    }

    pub fn add_item(&mut self, id: u32) {
        self.items.push(Item::new(id));
    }

    pub fn update_item_title(&mut self, id: u32, new_title: String) {
        self.items.with_id(id, |item| item.title = new_title);
    }
    pub fn update_item_app_id(&mut self, id: u32, new_app_id: String) {
        self.items.with_id(id, |item| item.app_id = new_app_id);
    }
    pub fn signal_item_activation(&mut self, id: u32) {
        if let Some(pos) = self.items.iter().position(|item| item.id == id) {
            self.items[..=pos].rotate_right(1);
        }
    }
    pub fn remove_item(&mut self, id: u32) {
        self.items.retain(|item| item.id != id);
    }
    pub fn get_first_item_id(&self) -> Option<u32> {
        self.items.first().map(|item| item.id)
    }
    pub fn update_item_preview(&mut self, id: u32, preview: (&[u8], usize)) {
        self.items.with_id(id, |item| {
            let (rgba, stride) = preview;
            let size = [stride / 4, rgba.len() / stride];
            let color_image = ColorImage::from_rgba_unmultiplied(size, rgba);

            if let Some(texture_handle) = &mut item.preview {
                texture_handle.set(color_image, Default::default());
            } else {
                item.preview = self
                    .egui_ctx
                    .load_texture(
                        format!("preview-{}-{}", item.id, item.app_id),
                        color_image,
                        Default::default(),
                    )
                    .into();
            };
        });
    }

    pub fn reset_selected_item(&mut self) {
        self.selected_item = cmp::min(1, self.items.len());
    }

    pub fn get_selected_item_id(&self) -> Option<u32> {
        self.items.get(self.selected_item).map(|item| item.id)
    }

    pub fn handle_events(&mut self, events: Vec<Event>) {
        for event in &events {
            if let Event::Key {
                key: egui::Key::Tab,
                pressed: true,
                modifiers,
                ..
            } = event
            {
                if !self.items.is_empty() {
                    self.selected_item += if modifiers.shift {
                        self.items.len() - 1
                    } else {
                        1
                    };
                    self.selected_item %= self.items.len();
                }
            }
        }

        let raw_input = RawInput {
            events,
            focused: true,
            ..Default::default()
        };
        self.build_output(raw_input);
    }

    fn build_output(&mut self, raw_input: RawInput) -> FullOutput {
        let full_output = self.egui_ctx.run(raw_input, |ctx: &Context| {
            let panel_frame = egui::Frame::new()
                .fill(egui::Color32::from_rgba_unmultiplied(25, 25, 25, 230))
                .corner_radius(10.0)
                .inner_margin(8.0);

            egui::CentralPanel::default()
                .frame(panel_frame)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.horizontal(|ui| {
                            for (index, item) in self.items.iter_mut().enumerate() {
                                egui::Frame::new()
                                    .stroke(if index == self.selected_item {
                                        egui::Stroke::new(2.0, egui::Color32::WHITE)
                                    } else {
                                        egui::Stroke::new(2.0, egui::Color32::TRANSPARENT)
                                    })
                                    .inner_margin(4.0)
                                    .show(ui, |ui| {
                                        ui.set_max_width(200.0);
                                        ui.set_max_height(100.0);
                                        ui.vertical(|ui| {
                                            ui.label(&item.title);

                                            if let Some(handle) = &item.preview {
                                                ui.image((handle.id(), (200.0, 100.0).into()));
                                            }
                                        });
                                    });
                            }
                        });
                    });
                });
        });

        self.needs_repaint = self.needs_repaint
            || full_output.viewport_output[&ViewportId::ROOT].repaint_delay != Duration::MAX;

        tracing::trace!(
            "repaint delay {:?}, cause {:?}",
            full_output.viewport_output[&ViewportId::ROOT].repaint_delay,
            self.egui_ctx.repaint_causes()
        );

        full_output
    }

    pub fn needs_repaint(&self) -> bool {
        self.needs_repaint
    }

    pub fn paint(&mut self, wgpu: &mut WgpuWrapper, wsurf: &mut WgpuSurface) -> anyhow::Result<()> {
        let _span = tracing::trace_span!("Paint").entered();

        let output = wsurf.surface.get_current_texture()?;

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let width = wsurf.surface_config.width;
        let height = wsurf.surface_config.height;

        // Build egui UI with collected events
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width as f32, height as f32),
            )),
            focused: true,
            ..Default::default()
        };

        let full_output = self.build_output(raw_input);

        let mut encoder = wgpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point: 1.0,
        };

        let clipped_primitives = self.egui_ctx.tessellate(full_output.shapes, 1.0);

        let egui_renderer = self.egui_renderer.get_or_insert_with(|| {
            egui_wgpu::Renderer::new(
                &wgpu.device,
                wsurf.surface_config.format,
                egui_wgpu::RendererOptions::default(),
            )
        });

        tracing::trace!("Updating textures");

        for (id, image_delta) in &full_output.textures_delta.set {
            egui_renderer.update_texture(&wgpu.device, &wgpu.queue, *id, image_delta);
        }

        tracing::trace!("Updating buffers");

        egui_renderer.update_buffers(
            &wgpu.device,
            &wgpu.queue,
            &mut encoder,
            &clipped_primitives,
            &screen_descriptor,
        );

        {
            tracing::trace!("Beginning render pass");

            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            egui_renderer.render(
                &mut render_pass.forget_lifetime(),
                &clipped_primitives,
                &screen_descriptor,
            );
        }

        tracing::trace!("Freeing textures");
        for id in &full_output.textures_delta.free {
            egui_renderer.free_texture(id);
        }

        tracing::trace!("Submitting queue");
        wgpu.queue.submit(iter::once(encoder.finish()));

        tracing::trace!("Presenting output");
        output.present();

        tracing::trace!("Completed");
        self.needs_repaint = false;

        Ok(())
    }
}
