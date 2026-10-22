use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::WsError;
use crate::fsops::write_atomic;
use crate::paths::same_path;

const SETTINGS_FILE: &str = "settings.json";
const PORTABLE_MARKER: &str = "kladde.portable";
const MAX_RECENT: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
}

/// Application settings; missing fields take defaults so older files keep loading.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub font_size: f32,
    pub tab_width: u8,
    pub insert_spaces: bool,
    pub show_whitespace: bool,
    pub word_wrap: bool,
    pub show_line_numbers: bool,
    pub large_file_mb: u64,
    pub backup_interval_secs: u64,
    pub history_enabled: bool,
    pub history_max_mb: u64,
    pub hex_bytes_per_row: u8,
    pub hex_group: u8,
    pub keybindings: BTreeMap<String, String>,
    pub recent_files: Vec<PathBuf>,
    /// Ask GitHub for a newer release at every start; off by default because Kladde otherwise stays offline.
    pub check_updates_on_start: bool,
    pub window_size: Option<[f32; 2]>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::System,
            font_size: 14.0,
            tab_width: 4,
            insert_spaces: true,
            show_whitespace: false,
            word_wrap: false,
            show_line_numbers: true,
            large_file_mb: 64,
            backup_interval_secs: 30,
            history_enabled: true,
            history_max_mb: 50,
            hex_bytes_per_row: 16,
            hex_group: 1,
            keybindings: BTreeMap::new(),
            recent_files: Vec::new(),
            check_updates_on_start: false,
            window_size: None,
        }
    }
}

fn push_recent(list: &mut Vec<PathBuf>, p: &Path) {
    list.retain(|e| !same_path(e, p));
    list.insert(0, p.to_path_buf());
    list.truncate(MAX_RECENT);
}

impl Settings {
    /// Reads `<dir>/settings.json`; defaults if the file does not exist.
    pub fn load(dir: &Path) -> Result<Settings, WsError> {
        let path = dir.join(SETTINGS_FILE);
        if path.try_exists()? {
            Ok(serde_json::from_str(&std::fs::read_to_string(&path)?)?)
        } else {
            Ok(Settings::default())
        }
    }

    pub fn save(&self, dir: &Path) -> Result<(), WsError> {
        std::fs::create_dir_all(dir)?;
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        write_atomic(&dir.join(SETTINGS_FILE), text.as_bytes())
    }

    /// Newest first, de-duplicated, at most 20 entries.
    pub fn push_recent_file(&mut self, p: &Path) {
        push_recent(&mut self.recent_files, p);
    }
}

fn portable_dir() -> Result<Option<PathBuf>, WsError> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| WsError::Invalid("executable has no parent directory".into()))?;
    Ok(dir.join(PORTABLE_MARKER).try_exists()?.then(|| dir.join("kladde-data")))
}

/// Settings/data directory, created if missing: `<exe dir>/kladde-data` when `kladde.portable` sits next to the
/// executable, else the platform data dir + `Kladde`.
pub fn data_dir() -> Result<PathBuf, WsError> {
    let dir = match portable_dir()? {
        Some(dir) => dir,
        None => dirs::data_dir().ok_or_else(|| WsError::Invalid("no platform data directory".into()))?.join("Kladde"),
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// True when the portable marker exists next to the executable; false if that cannot be determined.
pub fn is_portable() -> bool {
    matches!(portable_dir(), Ok(Some(_)))
}
