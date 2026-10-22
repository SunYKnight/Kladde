use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{find_program, hidden_command, ToolError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

/// Status of changed files in the repository containing `dir`, keyed by absolute path; `None` outside a repo.
/// Untracked folders are reported as one entry for the folder.
pub fn git_status(dir: &Path) -> Result<Option<HashMap<PathBuf, GitStatus>>, ToolError> {
    let git = find_program("git").ok_or_else(|| ToolError::NotFound("git".to_string()))?;
    if dir.is_dir() {
        repo_status(&git, dir)
    } else {
        Err(ToolError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("not a folder: {}", dir.display()),
        )))
    }
}

fn repo_status(git: &Path, dir: &Path) -> Result<Option<HashMap<PathBuf, GitStatus>>, ToolError> {
    let toplevel = git_in(git, dir).args(["rev-parse", "--show-toplevel"]).output()?;
    let stderr = String::from_utf8_lossy(&toplevel.stderr);
    if toplevel.status.success() {
        let top = native_path(String::from_utf8_lossy(&toplevel.stdout).trim());
        let out = crate::run_output(git_in(git, &top).args(["status", "--porcelain=v1", "-z"]))?;
        Ok(Some(parse_porcelain(&top, &String::from_utf8_lossy(&out))))
    } else if stderr.contains("not a git repository") {
        Ok(None)
    } else {
        Err(ToolError::Failed(format!("{} ({})", stderr.trim(), toplevel.status)))
    }
}

fn git_in(git: &Path, dir: &Path) -> Command {
    let mut cmd = hidden_command(git);
    // English messages, so "not a git repository" can be recognised.
    cmd.arg("-C").arg(dir).env("LC_ALL", "C").env("LANGUAGE", "C");
    cmd
}

fn native_path(git_path: &str) -> PathBuf {
    PathBuf::from(git_path.replace('/', std::path::MAIN_SEPARATOR_STR))
}

fn parse_porcelain(top: &Path, out: &str) -> HashMap<PathBuf, GitStatus> {
    let mut map = HashMap::new();
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    while let Some(entry) = fields.next() {
        let (code, path) = entry.split_at(entry.len().min(3));
        let xy = code.as_bytes();
        let (x, y) = (xy.first().copied().unwrap_or(b' '), xy.get(1).copied().unwrap_or(b' '));
        // Renames and copies are followed by a field with the original path.
        if x == b'R' || x == b'C' || y == b'R' || y == b'C' {
            fields.next();
        }
        if let Some(status) = classify(x, y) {
            map.insert(top.join(native_path(path.trim_end_matches('/'))), status);
        }
    }
    map
}

fn classify(x: u8, y: u8) -> Option<GitStatus> {
    match (x, y) {
        (b'?', b'?') => Some(GitStatus::Untracked),
        (b'!', b'!') => None,
        (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Some(GitStatus::Conflicted),
        (b'R', _) | (_, b'R') => Some(GitStatus::Renamed),
        (b'A', _) | (b'C', _) | (_, b'C') => Some(GitStatus::Added),
        (b'D', _) | (_, b'D') => Some(GitStatus::Deleted),
        _ => Some(GitStatus::Modified),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
    }

    #[test]
    fn status_in_temp_repo() {
        if find_program("git").is_none() {
            println!("git not installed; skipping git_status test");
        } else {
            let tmp = tempfile::tempdir().expect("temp dir");
            // Not canonicalized: old git rejects verbatim (\\?\) paths.
            let root = tmp.path().to_path_buf();
            let outside = git_status(&root).expect("status outside a repo");
            assert_eq!(outside, None);
            git(&root, &["init", "-q"]);
            for name in ["keep.txt", "change.txt", "remove.txt", "old.txt"] {
                std::fs::write(root.join(name), format!("{name}\n")).expect("write");
            }
            git(&root, &["add", "."]);
            git(&root, &["commit", "-q", "-m", "init"]);
            std::fs::write(root.join("change.txt"), "changed\n").expect("write");
            std::fs::remove_file(root.join("remove.txt")).expect("remove");
            git(&root, &["mv", "old.txt", "renamed.txt"]);
            std::fs::write(root.join("staged.txt"), "new\n").expect("write");
            git(&root, &["add", "staged.txt"]);
            std::fs::create_dir(root.join("sub")).expect("mkdir");
            std::fs::write(root.join("sub").join("new file.txt"), "x").expect("write");
            let status = git_status(&root.join("sub")).expect("status").expect("inside a repo");
            let rel = |p: &str| status.iter().find(|(k, _)| k.ends_with(p)).map(|(_, v)| *v);
            let changed = status.keys().find(|k| k.ends_with("change.txt")).expect("change.txt listed");
            assert!(same_file(changed, &root.join("change.txt")), "{changed:?}");
            assert_eq!(rel("change.txt"), Some(GitStatus::Modified));
            assert_eq!(rel("remove.txt"), Some(GitStatus::Deleted));
            assert_eq!(rel("renamed.txt"), Some(GitStatus::Renamed));
            assert_eq!(rel("staged.txt"), Some(GitStatus::Added));
            assert_eq!(rel("sub"), Some(GitStatus::Untracked));
            assert_eq!(rel("keep.txt"), None);
            assert_eq!(rel("old.txt"), None);
            assert!(status.keys().all(|k| k.is_absolute()), "{status:?}");
            assert!(matches!(git_status(&root.join("missing")), Err(ToolError::Io(_))));
        }
    }

    fn same_file(a: &Path, b: &Path) -> bool {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}
