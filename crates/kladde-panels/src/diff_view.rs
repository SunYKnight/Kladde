use egui::{Color32, Sense};
use kladde_tools::{DiffLine, DiffTag};

#[derive(Clone, Debug, PartialEq)]
pub struct DiffTheme {
    pub insert_bg: Color32,
    pub delete_bg: Color32,
    pub insert_fg: Color32,
    pub delete_fg: Color32,
    pub text: Color32,
    pub line_number: Color32,
}

impl DiffTheme {
    pub fn dark() -> Self {
        DiffTheme {
            insert_bg: Color32::from_rgb(28, 58, 34),
            delete_bg: Color32::from_rgb(70, 30, 32),
            insert_fg: Color32::from_rgb(120, 220, 130),
            delete_fg: Color32::from_rgb(240, 120, 120),
            text: Color32::from_rgb(212, 212, 212),
            line_number: Color32::from_rgb(120, 120, 120),
        }
    }

    pub fn light() -> Self {
        DiffTheme {
            insert_bg: Color32::from_rgb(220, 250, 222),
            delete_bg: Color32::from_rgb(255, 224, 224),
            insert_fg: Color32::from_rgb(20, 120, 40),
            delete_fg: Color32::from_rgb(170, 30, 30),
            text: Color32::from_rgb(30, 30, 30),
            line_number: Color32::from_rgb(140, 140, 140),
        }
    }
}

fn number(n: Option<usize>, width: usize) -> String {
    n.map_or_else(|| " ".repeat(width), |n| format!("{:>width$}", n + 1))
}

/// Unified diff with old and new line-number columns; only visible rows are laid out.
pub fn show_diff(ui: &mut egui::Ui, lines: &[DiffLine], theme: &DiffTheme) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.fonts(|f| f.row_height(&font));
    let char_w = ui.fonts(|f| f.glyph_width(&font, '0'));
    let max_no = lines.iter().filter_map(|l| l.old_line.max(l.new_line)).max().unwrap_or(0) + 1;
    let digits = max_no.to_string().len();
    let longest = lines.iter().map(|l| l.text.chars().count()).max().unwrap_or(0);
    let gutter_chars = digits * 2 + 4;
    let content_w = (gutter_chars + longest) as f32 * char_w + 16.0;
    egui::ScrollArea::both().id_salt("kladde_diff").auto_shrink([false, false]).show_rows(
        ui,
        row_h,
        lines.len(),
        |ui, range| {
            lines[range].iter().for_each(|l| {
                let width = ui.available_width().max(content_w);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(width, row_h), Sense::hover());
                let (bg, fg, mark) = match l.tag {
                    DiffTag::Equal => (Color32::TRANSPARENT, theme.text, ' '),
                    DiffTag::Insert => (theme.insert_bg, theme.insert_fg, '+'),
                    DiffTag::Delete => (theme.delete_bg, theme.delete_fg, '-'),
                };
                let painter = ui.painter();
                painter.rect_filled(rect, 0.0, bg);
                let gutter = format!("{} {} {mark} ", number(l.old_line, digits), number(l.new_line, digits));
                let left = rect.left_top();
                painter.text(left, egui::Align2::LEFT_TOP, gutter, font.clone(), theme.line_number);
                let text = l.text.replace('\t', "    ");
                let at = left + egui::vec2(gutter_chars as f32 * char_w, 0.0);
                painter.text(at, egui::Align2::LEFT_TOP, text, font.clone(), fg);
            });
        },
    );
}
