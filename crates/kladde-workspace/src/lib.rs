//! Workspace model, settings, session and fuzzy matching for Kladde.

mod error;
mod fsops;
mod fuzzy;
mod model;
mod paths;
mod session;
mod settings;

pub use error::WsError;
pub use fsops::{list_dir, rename_path_on_disk, DirEntry};
pub use fuzzy::{fuzzy_rank, fuzzy_score};
pub use model::{Node, NodeId, NodeKind, Rgb, Workspace, PALETTE};
pub use session::{Session, SplitMode, TabState, ViewKind};
pub use settings::{data_dir, is_portable, Settings, ThemeChoice};
