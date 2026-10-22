use egui::Color32;

/// Colors of the hex view.
#[derive(Clone, Debug, PartialEq)]
pub struct HexTheme {
    pub background: Color32,
    pub header: Color32,
    pub offset: Color32,
    pub byte: Color32,
    pub zero_byte: Color32,
    pub text: Color32,
    pub non_printable: Color32,
    pub selection: Color32,
    pub caret: Color32,
    pub caret_inactive: Color32,
    pub cursor_row: Color32,
    pub scrollbar_track: Color32,
    pub scrollbar_thumb: Color32,
}

impl HexTheme {
    pub fn dark() -> Self {
        HexTheme {
            background: Color32::from_rgb(0x1E, 0x1E, 0x1E),
            header: Color32::from_rgb(0x56, 0x9C, 0xD6),
            offset: Color32::from_rgb(0x85, 0x85, 0x85),
            byte: Color32::from_rgb(0xD4, 0xD4, 0xD4),
            zero_byte: Color32::from_rgb(0x5A, 0x5A, 0x5A),
            text: Color32::from_rgb(0xCE, 0x91, 0x78),
            non_printable: Color32::from_rgb(0x5A, 0x5A, 0x5A),
            selection: Color32::from_rgb(0x26, 0x4F, 0x78),
            caret: Color32::from_rgb(0xFF, 0xCC, 0x00),
            caret_inactive: Color32::from_rgb(0x80, 0x80, 0x80),
            cursor_row: Color32::from_rgb(0x2A, 0x2A, 0x2A),
            scrollbar_track: Color32::from_rgb(0x25, 0x25, 0x25),
            scrollbar_thumb: Color32::from_rgb(0x4F, 0x4F, 0x4F),
        }
    }

    pub fn light() -> Self {
        HexTheme {
            background: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            header: Color32::from_rgb(0x00, 0x55, 0xAA),
            offset: Color32::from_rgb(0x80, 0x80, 0x80),
            byte: Color32::from_rgb(0x1E, 0x1E, 0x1E),
            zero_byte: Color32::from_rgb(0xB0, 0xB0, 0xB0),
            text: Color32::from_rgb(0xA3, 0x15, 0x15),
            non_printable: Color32::from_rgb(0xB0, 0xB0, 0xB0),
            selection: Color32::from_rgb(0xAD, 0xD6, 0xFF),
            caret: Color32::from_rgb(0xE0, 0x6C, 0x00),
            caret_inactive: Color32::from_rgb(0xA0, 0xA0, 0xA0),
            cursor_row: Color32::from_rgb(0xF2, 0xF2, 0xF2),
            scrollbar_track: Color32::from_rgb(0xF0, 0xF0, 0xF0),
            scrollbar_thumb: Color32::from_rgb(0xC1, 0xC1, 0xC1),
        }
    }
}
