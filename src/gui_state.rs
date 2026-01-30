use std::borrow::Cow;

use egui::{Color32, ColorImage, Pos2, Rect, TextureHandle, hex_color};

#[derive(Default)]
pub struct Item {
    id: u32,
    title: String,
    app_id: String,
    preview: Option<(TextureHandle, [usize; 2])>,
}

impl Item {
    fn new(id: u32) -> Self {
        Self {
            id,
            ..Default::default()
        }
    }

    pub fn get_preview(&self) -> &Option<(TextureHandle, [usize; 2])> {
        &self.preview
    }

    pub fn get_app_id(&self) -> &str {
        &self.app_id
    }

    pub fn get_title(&self) -> Cow<'_, str> {
        if self.app_id.is_empty() {
            if self.title.is_empty() {
                // TODO: maybe we shouldn't show windows that don't have an app id and title?
                // fix if someone complains about it
                return "Untitled Window".into();
            }
            return self.title.as_str().into();
        }
        format!(
            "{} | {}{}",
            &self.title,
            self.app_id[..1].to_uppercase(),
            &self.app_id.get(1..).unwrap_or_default()
        )
        .into()
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

pub struct LayoutParams {
    window_max_width: u32,
    pub window_corner_radius: f32,
    pub window_padding: u32,
    pub window_background: Color32,
    items_gap: u32,
    pub item_stroke: u32,
    pub item_padding: u32,
    pub item_corner_radius: f32,
    pub item_hover_background: Color32,
    pub item_active_background: Color32,
    pub title_height: u32,
    preview_height: u32,
    preview_min_width: u32,
    preview_max_width: u32,
    pub preview_corner_radius: f32,
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self {
            window_max_width: 800,
            window_corner_radius: 6.0,
            window_padding: 10,
            window_background: hex_color!("#20202044"),
            items_gap: 10,
            item_stroke: 0,
            item_padding: 7,
            item_corner_radius: 6.0,
            item_hover_background: hex_color!("#11111144"),
            item_active_background: hex_color!("#11111177"),
            title_height: 25,
            preview_height: 100,
            preview_min_width: 100,
            preview_max_width: 200,
            preview_corner_radius: 3.0,
        }
    }
}

#[derive(Default)]
pub struct LayoutComputed {
    pub window_height: u32,
    pub window_width: u32,
    pub item_rects: Vec<Rect>,
}

pub struct LayoutResult<'a> {
    pub items: &'a [Item],
    pub selected_item: usize,
    pub hovered_item: Option<usize>,
    pub params: &'a LayoutParams,
    pub computed: &'a LayoutComputed,
}

#[derive(Default)]
pub struct GuiState {
    items: Vec<Item>,
    selected_item: usize,
    hovered_item: Option<usize>,
    needs_repaint: bool,
    layout_params: LayoutParams,
    layout_computed: LayoutComputed,
}

impl GuiState {
    pub fn add_item(&mut self, id: u32) {
        self.items.push(Item::new(id));
    }

    pub fn update_item_title(&mut self, id: u32, new_title: String) {
        self.items.with_id(id, |item| item.title = new_title);
        self.needs_repaint = true;
    }
    pub fn update_item_app_id(&mut self, id: u32, new_app_id: String) {
        self.items.with_id(id, |item| item.app_id = new_app_id);
        self.needs_repaint = true;
    }
    pub fn signal_item_activation(&mut self, id: u32) {
        if let Some(pos) = self.items.iter().position(|item| item.id == id) {
            self.items[..=pos].rotate_right(1);
            self.needs_repaint = true;
        }
    }
    pub fn remove_item(&mut self, id: u32) {
        self.items.retain(|item| item.id != id);
        self.needs_repaint = true;
    }
    pub fn get_first_item_id(&self) -> Option<u32> {
        self.items.first().map(|item| item.id)
    }
    pub fn update_item_preview(
        &mut self,
        id: u32,
        preview: (&[u8], usize),
        load_texture: impl FnOnce(String, ColorImage) -> TextureHandle,
    ) {
        self.items.with_id(id, |item| {
            let (rgba, width) = preview;
            let image_size = [width, rgba.len() / width / 4];
            let color_image = ColorImage::from_rgba_unmultiplied(image_size, rgba);

            if let Some((texture_handle, size)) = &mut item.preview {
                texture_handle.set(color_image, Default::default());
                *size = image_size;
            } else {
                item.preview = (
                    load_texture(format!("preview-{}-{}", item.id, item.app_id), color_image),
                    image_size,
                )
                    .into();
            };
            self.needs_repaint = true;
        });
    }
    pub fn calculate_preview_size(&self, original_size: (u32, u32)) -> (u32, u32) {
        let (original_width, original_height) = original_size;
        let preview_height = self.layout_params.preview_height;
        let preview_width = original_width * preview_height / original_height;
        let preview_width = preview_width.clamp(
            self.layout_params.preview_min_width,
            self.layout_params.preview_max_width,
        );
        (preview_width, preview_height)
    }

    pub fn reset_selected_item(&mut self) {
        self.selected_item = self.items.len().min(1);
        self.needs_repaint = true;
    }

    pub fn get_selected_item_id(&self) -> Option<u32> {
        self.items.get(self.selected_item).map(|item| item.id)
    }
    pub fn select_next_item(&mut self) {
        if self.items.len() == 0 {
            return;
        }

        self.selected_item = (self.selected_item + 1) % self.items.len();
        self.needs_repaint = true;
    }
    pub fn select_previous_item(&mut self) {
        if self.items.len() == 0 {
            return;
        }

        self.selected_item = (self.selected_item + self.items.len() - 1) % self.items.len();
        self.needs_repaint = true;
    }
    pub fn set_hovered_item(&mut self, index: Option<usize>) {
        if self.hovered_item != index {
            self.hovered_item = index;
            self.needs_repaint = true;
        }
    }
    pub fn needs_repaint(&self) -> bool {
        self.needs_repaint
    }
    pub fn mark_repainted(&mut self) {
        self.needs_repaint = false;
    }

    fn get_item_width(&self, item: &Item) -> u32 {
        let content_width = match item.preview {
            Some((_, [width, _])) => width as u32,
            _ => self.layout_params.preview_min_width,
        };
        content_width + self.layout_params.item_stroke * 2 + self.layout_params.item_padding * 2
    }

    fn get_item_height(&self) -> u32 {
        self.layout_params.title_height
            + self.layout_params.preview_height
            + self.layout_params.item_stroke * 2
            + self.layout_params.item_padding * 2
    }

    // Calculate layout
    pub fn calculate_layout(&mut self) -> LayoutResult<'_> {
        self.layout_computed = Default::default();

        let available_row_width =
            self.layout_params.window_max_width - self.layout_params.window_padding * 2;
        let mut longest_row_width = 0;

        let mut rows: Vec<(Vec<u32>, u32)> = Vec::new();

        for item in self.items.iter() {
            let item_width = self.get_item_width(item);
            let needed_width = self.layout_params.items_gap + item_width;

            if let Some((row, row_width)) = rows.last_mut()
                && *row_width + needed_width <= available_row_width
            {
                row.push(item_width);
                *row_width += needed_width;
                longest_row_width = longest_row_width.max(*row_width);
                continue;
            }

            rows.push((vec![item_width], item_width));
            longest_row_width = longest_row_width.max(item_width);
        }

        let row_count = rows.len() as i32;

        let window_width = longest_row_width + self.layout_params.window_padding * 2;
        let window_height = row_count as u32 * self.get_item_height()
            + (row_count - 1).max(0) as u32 * self.layout_params.items_gap
            + self.layout_params.window_padding * 2;

        let mut item_rects = Vec::new();

        let x = self.layout_params.window_padding as f32;
        let mut y = self.layout_params.window_padding as f32;
        let row_height = self.get_item_height() as f32;
        for (row, row_width) in rows.into_iter() {
            let mut x = (longest_row_width - row_width) as f32 / 2.0 + x;

            for item_width in row.into_iter() {
                let rect = Rect {
                    min: Pos2 { x, y },
                    max: Pos2 {
                        x: x + item_width as f32,
                        y: y + row_height,
                    },
                };
                x += (item_width + self.layout_params.items_gap) as f32;
                item_rects.push(rect);
            }
            y += row_height + self.layout_params.items_gap as f32;
        }

        self.layout_computed = LayoutComputed {
            window_height,
            window_width,
            item_rects,
        };

        LayoutResult {
            items: &self.items,
            selected_item: self.selected_item,
            hovered_item: self.hovered_item,
            params: &self.layout_params,
            computed: &self.layout_computed,
        }
    }
}
