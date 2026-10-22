use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::WsError;
use crate::fsops::write_atomic;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SplitMode {
    #[default]
    None,
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ViewKind {
    #[default]
    Text,
    Hex,
}

/// One open tab; `backup` holds unsaved content for hot exit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TabState {
    pub path: Option<PathBuf>,
    pub backup: Option<PathBuf>,
    pub title: String,
    pub pane: u8,
    pub view: ViewKind,
    pub caret: u64,
    pub scroll: u64,
    pub read_only: bool,
    pub tail: bool,
}

/// Open tabs and layout of one workspace (or of the app without workspace).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<TabState>,
    pub active: [Option<usize>; 2],
    pub split: SplitMode,
    pub split_ratio: f32,
}

impl Session {
    pub fn load(path: &Path) -> Result<Session, WsError> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), WsError> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        write_atomic(path, text.as_bytes())
    }
}
