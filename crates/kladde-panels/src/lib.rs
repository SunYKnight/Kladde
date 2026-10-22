//! Kladde panels: find, results, output, history, diff, csv, palette, settings, commands, jobs.

mod commands;
mod csv_view;
mod diff_view;
mod find;
mod history;
mod jobs;
mod output;
mod palette;
mod results;
mod settings;

pub use commands::{
    commands, encoding_command, encoding_for_command, line_tool_command, line_tool_for_command, Chord, CommandSpec,
    Keymap,
};
pub use csv_view::show_csv;
pub use diff_view::{show_diff, DiffTheme};
pub use find::{parse_hex, show_find_bar, FindAction, FindScope, FindState};
pub use history::{format_utc, show_history, HistoryEvent, HistoryState};
pub use jobs::{file_matches, DocSearchJob, FileMatch, FilesSearchJob, HashJob};
pub use output::{show_output, OutputEvent, OutputState};
pub use palette::{show_palette, PaletteItem, PaletteState};
pub use results::{show_results, ResultsEvent, ResultsState};
pub use settings::{settings_capturing, show_settings};
