use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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

/// Open tabs and layout of one workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<TabState>,
    pub active: [Option<usize>; 2],
    pub split: SplitMode,
    pub split_ratio: f32,
}

impl Session {
    /// Removes tab `i`; the active tab of its pane stays the same tab where it still exists.
    pub fn remove_tab(&mut self, i: usize) -> Option<TabState> {
        (i < self.tabs.len()).then(|| {
            let tab = self.tabs.remove(i);
            let pane = tab.pane.min(1);
            let in_pane = |t: &&TabState| t.pane.min(1) == pane;
            let before = self.tabs.iter().take(i).filter(in_pane).count();
            let left = self.tabs.iter().filter(in_pane).count();
            let slot = &mut self.active[usize::from(pane)];
            *slot = match *slot {
                _ if left == 0 => None,
                Some(a) if a > before => Some(a - 1),
                other => other.map(|a| a.min(left - 1)),
            };
            tab
        })
    }

    /// Appends `tab` to the first pane; it becomes active only in an empty pane.
    pub fn push_tab(&mut self, tab: TabState) {
        let index = self.tabs.iter().filter(|t| t.pane == 0).count();
        self.tabs.push(TabState { pane: 0, ..tab });
        self.active[0] = self.active[0].or(Some(index));
    }
}
