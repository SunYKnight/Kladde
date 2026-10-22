//! External processes (script runner, Quarto, git), file history and text transforms for Kladde.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

mod csv_table;
mod diff;
mod git;
mod hash;
mod history;
mod process_tree;
mod quarto;
mod runner;
mod transform;

pub use csv_table::{parse_csv, CsvTable};
pub use diff::{diff_text, DiffLine, DiffTag};
pub use git::{git_status, GitStatus};
pub use hash::{HashAlgo, StreamHasher};
pub use history::{History, Version};
pub use quarto::{can_render, quarto_output_file, quarto_path, quarto_render};
pub use runner::{find_fences, run_file, run_in_terminal, run_script, FenceBlock, Interpreter, RunEvent, RunHandle};
pub use transform::{
    apply_line_tool, base64_decode, base64_encode, hex_decode, hex_encode, json_minify, json_pretty, url_decode,
    url_encode, xml_minify, xml_pretty, LineTool,
};

#[derive(Debug)]
pub enum ToolError {
    Io(std::io::Error),
    NotFound(String),
    Failed(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::Io(e) => write!(f, "{e}"),
            ToolError::NotFound(what) => write!(f, "not found: {what}"),
            ToolError::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ToolError::Io(e) => Some(e),
            ToolError::NotFound(_) | ToolError::Failed(_) => None,
        }
    }
}

impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        ToolError::Io(e)
    }
}

/// Searches `PATH` for an executable (`PATHEXT` extensions on Windows). Names with a directory part are checked
/// as given.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let given = Path::new(name);
    let has_dir = given.components().count() > 1 || given.is_absolute();
    if has_dir {
        candidates(given).into_iter().find(|p| is_executable(p))
    } else {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .filter(|dir| !dir.as_os_str().is_empty())
                .find_map(|dir| candidates(&dir.join(name)).into_iter().find(|p| is_executable(p)))
        })
    }
}

#[cfg(windows)]
fn candidates(base: &Path) -> Vec<PathBuf> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let exts: Vec<&str> = exts.split(';').filter(|e| !e.is_empty()).collect();
    let current = base.extension().map(|e| format!(".{}", e.to_string_lossy()));
    let has_known_ext = current.as_deref().is_some_and(|c| exts.iter().any(|e| e.eq_ignore_ascii_case(c)));
    if has_known_ext {
        vec![base.to_path_buf()]
    } else {
        exts.iter()
            .map(|ext| {
                let mut name = base.as_os_str().to_os_string();
                name.push(ext.to_ascii_lowercase());
                PathBuf::from(name)
            })
            .collect()
    }
}

#[cfg(not(windows))]
fn candidates(base: &Path) -> Vec<PathBuf> {
    vec![base.to_path_buf()]
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Opens a file or folder with the application the OS associates with it.
#[cfg(windows)]
pub fn open_with_os(path: &Path) -> Result<(), ToolError> {
    use std::os::windows::process::CommandExt;
    // Explorer splits unquoted arguments at commas.
    let mut quoted = OsString::from("\"");
    quoted.push(path.as_os_str());
    quoted.push("\"");
    Command::new("explorer").raw_arg(quoted).spawn()?;
    Ok(())
}

/// Opens a file or folder with the application the OS associates with it.
#[cfg(not(windows))]
pub fn open_with_os(path: &Path) -> Result<(), ToolError> {
    spawn_detached(&[OsString::from("xdg-open"), path.as_os_str().to_os_string()], None)
}

/// Starts `argv` in the background via `sh` so the child is reparented and never becomes a zombie of the app.
#[cfg(not(windows))]
pub(crate) fn spawn_detached(argv: &[OsString], cwd: Option<&Path>) -> Result<(), ToolError> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg("\"$@\" >/dev/null 2>&1 &").arg("kladde").args(argv);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(ToolError::Failed(format!("could not start {}: {status}", argv[0].to_string_lossy())))
    }
}

/// A command that never flashes a console window on Windows.
pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    hide_window(&mut cmd);
    cmd
}

#[cfg(windows)]
pub(crate) fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn hide_window(_cmd: &mut Command) {}

/// Runs a command to completion and returns stdout, or `Failed` with stderr on a non-zero exit.
pub(crate) fn run_output(cmd: &mut Command) -> Result<Vec<u8>, ToolError> {
    let out = cmd.output()?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(ToolError::Failed(format!("{} ({})", stderr, out.status)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_program_finds_platform_shell_and_rejects_unknown() {
        let shell = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(find_program(shell).is_some_and(|p| p.is_file()));
        assert!(find_program("kladde-no-such-program-xyz").is_none());
    }

    #[test]
    fn find_program_accepts_explicit_path() {
        let shell = find_program(if cfg!(windows) { "cmd" } else { "sh" }).expect("platform shell is on PATH");
        assert_eq!(find_program(&shell.to_string_lossy()), Some(shell));
    }
}
