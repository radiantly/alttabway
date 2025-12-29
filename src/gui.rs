use egui::Context;

use crate::wgpu_wrapper::WgpuWrapper;

#[derive(Default, Debug)]
pub struct Gui {
    egui_ctx: Context,
}

impl Gui {
    pub fn new() -> Self {
        Gui::default()
    }

    pub fn render(&mut self, wgpu: &mut WgpuWrapper) -> anyhow::Result<()> {
        tracing::info!("render() called");

        let output = wgpu.surface.get_current_texture()?;
        tracing::info!("Got texture, rendering...");

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let width = wgpu.surface_config.width;
        let height = wgpu.surface_config.height;

        // Build egui UI
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width as f32, height as f32),
            )),
            ..Default::default()
        };

        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Alt-Tab for Wayland");
                ui.label("Hello from egui!");
            });
        });

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

        for (id, image_delta) in &full_output.textures_delta.set {
            wgpu.egui_renderer
                .update_texture(&wgpu.device, &wgpu.queue, *id, image_delta);
        }

        wgpu.egui_renderer.update_buffers(
            &wgpu.device,
            &wgpu.queue,
            &mut encoder,
            &clipped_primitives,
            &screen_descriptor,
        );

        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1,
                            g: 0.1,
                            b: 0.1,
                            a: 0.9,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            wgpu.egui_renderer.render(
                &mut render_pass.forget_lifetime(),
                &clipped_primitives,
                &screen_descriptor,
            );
        }

        for id in &full_output.textures_delta.free {
            wgpu.egui_renderer.free_texture(id);
        }

        wgpu.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }
}
