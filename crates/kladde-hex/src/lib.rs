//! Hex/binary view with overwrite/insert editing and a data inspector for kladde documents.

mod format;
mod inspector;
mod state;
mod theme;
mod view;

pub use inspector::show_inspector;
pub use state::HexState;
pub use theme::HexTheme;
pub use view::show;

/// What the byte columns show: two hex digits or eight bits per byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HexMode {
    Hex,
    Binary,
}

/// Decoding of the text column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextColumn {
    Ascii,
    Latin1,
    Utf8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HexConfig {
    pub bytes_per_row: u8,
    /// Bytes per group; groups are separated by an extra gap (1 = no grouping).
    pub group: u8,
    pub mode: HexMode,
    pub text: TextColumn,
    pub font_size: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HexEvent {
    Error(String),
}

pub struct HexOutput {
    pub events: Vec<HexEvent>,
    /// The document content changed this frame.
    pub changed: bool,
    pub focused: bool,
}
