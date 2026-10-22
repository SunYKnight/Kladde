use egui::{Align2, Color32, CursorIcon, Rect, Sense, Stroke};
use kladde_tools::CsvTable;

const MIN_COL: f32 = 30.0;
const MAX_AUTO_COL: f32 = 320.0;
const PAD: f32 = 4.0;
/// Rows sampled to size columns initially.
const SIZE_SAMPLE: usize = 200;

fn initial_widths(table: &CsvTable, char_w: f32) -> Vec<f32> {
    let cols = table.rows.iter().map(Vec::len).max().unwrap_or(0);
    (0..cols)
        .map(|c| {
            let chars = table.rows.iter().take(SIZE_SAMPLE).filter_map(|r| r.get(c)).map(|s| s.chars().count()).max();
            (chars.unwrap_or(0) as f32 * char_w + 2.0 * PAD).clamp(MIN_COL, MAX_AUTO_COL)
        })
        .collect()
}

fn paint_cell(ui: &egui::Ui, rect: Rect, text: &str, font: &egui::FontId, color: Color32) {
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let shown: String = text.chars().take(400).map(|c| if c == '\n' || c == '\r' { '↵' } else { c }).collect();
    painter.text(rect.left_center() + egui::vec2(PAD, 0.0), Align2::LEFT_CENTER, shown, font.clone(), color);
}

/// First row as header, columns resizable by dragging the header borders, rows virtualized.
pub fn show_csv(ui: &mut egui::Ui, table: &CsvTable) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.fonts(|f| f.row_height(&font)) + 4.0;
    let char_w = ui.fonts(|f| f.glyph_width(&font, '0'));
    let id = ui.id().with("kladde_csv_widths");
    let mut widths: Vec<f32> = ui.data_mut(|d| d.get_temp(id)).unwrap_or_default();
    let cols = table.rows.iter().map(Vec::len).max().unwrap_or(0);
    if widths.len() != cols {
        widths = initial_widths(table, char_w);
    }
    let data_rows = table.rows.len().saturating_sub(1);
    let gutter = (data_rows.max(1).to_string().len() as f32 + 1.0) * char_w + 2.0 * PAD;
    let total_w = gutter + widths.iter().sum::<f32>();
    let visuals = ui.visuals().clone();
    let grid = Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color);
    let header = table.rows.first().cloned().unwrap_or_default();
    egui::ScrollArea::horizontal().id_salt("kladde_csv_h").auto_shrink([false, false]).show(ui, |ui| {
        let width = ui.available_width().max(total_w);
        let (head_rect, _) = ui.allocate_exact_size(egui::vec2(width, row_h), Sense::hover());
        ui.painter().rect_filled(head_rect, 0.0, visuals.faint_bg_color);
        let mut x = head_rect.left() + gutter;
        (0..cols).for_each(|c| {
            let cell = Rect::from_min_size(egui::pos2(x, head_rect.top()), egui::vec2(widths[c], row_h));
            let name = header.get(c).map_or("", String::as_str);
            paint_cell(ui, cell, name, &font, visuals.strong_text_color());
            let handle = Rect::from_x_y_ranges(cell.right() - 3.0..=cell.right() + 3.0, cell.y_range());
            let drag = ui.interact(handle, id.with(c), Sense::drag()).on_hover_cursor(CursorIcon::ResizeHorizontal);
            if drag.dragged() {
                widths[c] = (widths[c] + drag.drag_delta().x).max(MIN_COL);
            }
            ui.painter().vline(cell.right(), head_rect.y_range(), grid);
            x += widths[c];
        });
        egui::ScrollArea::vertical().id_salt("kladde_csv_v").auto_shrink([false, false]).show_rows(
            ui,
            row_h,
            data_rows,
            |ui, range| {
                range.for_each(|r| {
                    let row = &table.rows[r + 1];
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, row_h), Sense::hover());
                    if r % 2 == 1 {
                        ui.painter().rect_filled(rect, 0.0, visuals.faint_bg_color);
                    }
                    let gutter_rect = Rect::from_min_size(rect.min, egui::vec2(gutter, row_h));
                    paint_cell(ui, gutter_rect, &(r + 1).to_string(), &font, visuals.weak_text_color());
                    let mut x = rect.left() + gutter;
                    (0..cols).for_each(|c| {
                        let cell = Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(widths[c], row_h));
                        paint_cell(ui, cell, row.get(c).map_or("", String::as_str), &font, visuals.text_color());
                        ui.painter().vline(cell.right(), rect.y_range(), grid);
                        x += widths[c];
                    });
                });
            },
        );
    });
    ui.data_mut(|d| d.insert_temp(id, widths));
}
