use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};

use crate::{find_program, hidden_command, run_output, ToolError};

/// Name of the single blob inside every history commit's tree.
const ENTRY_NAME: &str = "content";
const IDENTITY_NAME: &str = "Kladde";
const IDENTITY_EMAIL: &str = "kladde@localhost";

/// One stored version of a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Version {
    pub commit: String,
    pub blob: String,
    /// Unix seconds.
    pub time: i64,
    pub size: u64,
}

/// File versions in a private bare git repository; one ref per file, one blob per commit.
pub struct History {
    git: PathBuf,
    git_dir: PathBuf,
}

impl History {
    /// Opens (and on first use creates) `<data_dir>/history.git`; `None` if git is not installed.
    pub fn open(data_dir: &Path) -> Result<Option<History>, ToolError> {
        let history = find_program("git").map(|git| History { git, git_dir: data_dir.join("history.git") });
        if let Some(h) = &history {
            if !h.git_dir.join("HEAD").is_file() {
                std::fs::create_dir_all(&h.git_dir)?;
                h.git(&["init", "--bare", "--quiet"])?;
            }
        }
        Ok(history)
    }

    /// Stores the file's current content; `None` if it equals the newest stored version.
    pub fn snapshot(&self, file: &Path) -> Result<Option<Version>, ToolError> {
        let key = ref_name(&canonical(file)?);
        let mut hash_args: Vec<OsString> = ["hash-object", "-w", "--no-filters", "--"].map(OsString::from).into();
        hash_args.push(file.as_os_str().to_os_string());
        let blob = self.git_line(&hash_args)?;
        let tree = String::from_utf8_lossy(
            &self.git_stdin(&["mktree"], format!("100644 blob {blob}\t{ENTRY_NAME}\n").as_bytes())?,
        )
        .trim()
        .to_string();
        let head = self.head(&key)?;
        let unchanged = head.as_ref().is_some_and(|(_, head_tree)| *head_tree == tree);
        let version = if unchanged { None } else { Some(self.commit(&key, &blob, &tree, head)?) };
        Ok(version)
    }

    /// Stored versions of `file`, newest first.
    pub fn versions(&self, file: &Path) -> Result<Vec<Version>, ToolError> {
        let key = ref_name(&canonical(file)?);
        let versions = match self.head(&key)? {
            None => Vec::new(),
            Some((commit, _)) => {
                let log =
                    self.git_line(&["log", "--no-show-signature", "--format=%H %ct %s", commit.as_str(), "--"])?;
                log.lines().map(parse_log_line).collect::<Result<Vec<_>, _>>()?
            }
        };
        Ok(versions)
    }

    pub fn read(&self, v: &Version) -> Result<Vec<u8>, ToolError> {
        self.git(&["cat-file", "blob", &v.blob])
    }

    /// Moves the history of `old` to `new` after the file was renamed on disk.
    pub fn rename(&self, old: &Path, new: &Path) -> Result<(), ToolError> {
        let old_key = ref_name(&canonical(old)?);
        let new_key = ref_name(&canonical(new)?);
        if let (Some((commit, _)), true) = (self.head(&old_key)?, old_key != new_key) {
            self.git(&["update-ref", &new_key, &commit])?;
            self.git(&["update-ref", "-d", &old_key, &commit])?;
        }
        Ok(())
    }

    /// Newest (commit, tree) of a ref, if the ref exists.
    fn head(&self, key: &str) -> Result<Option<(String, String)>, ToolError> {
        let line = self.git_line(&["for-each-ref", "--format=%(objectname) %(tree)", key])?;
        Ok(line.split_once(' ').map(|(c, t)| (c.to_string(), t.trim().to_string())))
    }

    fn commit(&self, key: &str, blob: &str, tree: &str, head: Option<(String, String)>) -> Result<Version, ToolError> {
        let size: u64 = self
            .git_line(&["cat-file", "-s", blob])?
            .parse()
            .map_err(|e| ToolError::Failed(format!("unexpected blob size from git: {e}")))?;
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| ToolError::Failed(format!("system clock before 1970: {e}")))?
            .as_secs() as i64;
        // Subject holds blob and size: `versions` needs one `git log`.
        let message = format!("{blob} {size}");
        let mut args = vec!["commit-tree", "--no-gpg-sign", tree, "-m", message.as_str()];
        if let Some((parent, _)) = &head {
            args.extend(["-p", parent.as_str()]);
        }
        let date = format!("{time} +0000");
        let mut cmd = self.command(&args);
        cmd.env("GIT_AUTHOR_DATE", &date).env("GIT_COMMITTER_DATE", &date);
        let commit = String::from_utf8_lossy(&run_output(&mut cmd)?).trim().to_string();
        let mut update = vec!["update-ref", key, commit.as_str()];
        if let Some((parent, _)) = &head {
            update.push(parent.as_str());
        }
        self.git(&update)?;
        Ok(Version { commit, blob: blob.to_string(), time, size })
    }

    /// Every git call is pinned to the history repo and a fixed identity, independent of the user's setup.
    fn command<S: AsRef<OsStr>>(&self, args: &[S]) -> Command {
        let mut git_dir = OsString::from("--git-dir=");
        git_dir.push(&self.git_dir);
        let mut cmd = hidden_command(&self.git);
        // exFAT/FAT (portable mode) has no owner; git calls that dubious.
        cmd.arg(git_dir).args(["-c", "safe.directory=*", "-c", "gc.auto=0"]).args(args);
        cmd.env("GIT_AUTHOR_NAME", IDENTITY_NAME)
            .env("GIT_AUTHOR_EMAIL", IDENTITY_EMAIL)
            .env("GIT_COMMITTER_NAME", IDENTITY_NAME)
            .env("GIT_COMMITTER_EMAIL", IDENTITY_EMAIL)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null());
        cmd
    }

    fn git<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Vec<u8>, ToolError> {
        run_output(&mut self.command(args))
    }

    fn git_line<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<String, ToolError> {
        Ok(String::from_utf8_lossy(&self.git(args)?).trim().to_string())
    }

    fn git_stdin(&self, args: &[&str], input: &[u8]) -> Result<Vec<u8>, ToolError> {
        let mut cmd = self.command(args);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        child.stdin.take().expect("stdin was configured as piped").write_all(input)?;
        let out = child.wait_with_output()?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(ToolError::Failed(format!("{} ({})", String::from_utf8_lossy(&out.stderr).trim(), out.status)))
        }
    }
}

fn parse_log_line(line: &str) -> Result<Version, ToolError> {
    let bad = || ToolError::Failed(format!("unexpected history entry: {line}"));
    let mut parts = line.split_whitespace();
    let commit = parts.next().ok_or_else(bad)?.to_string();
    let time = parts.next().and_then(|t| t.parse().ok()).ok_or_else(bad)?;
    let blob = parts.next().ok_or_else(bad)?.to_string();
    let size = parts.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
    Ok(Version { commit, blob, time, size })
}

/// Canonical form of a path that may no longer exist (the old name after a rename).
fn canonical(path: &Path) -> Result<PathBuf, ToolError> {
    let resolved = match std::fs::canonicalize(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let name =
                path.file_name().ok_or_else(|| ToolError::Failed(format!("no file name: {}", path.display())))?;
            let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            std::fs::canonicalize(parent).map(|p| p.join(name))
        }
        other => other,
    }?;
    Ok(resolved)
}

fn ref_name(canonical: &Path) -> String {
    let digest = Sha1::digest(canonical.to_string_lossy().as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("refs/kladde/{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(dir: &Path) -> Option<History> {
        let history = History::open(dir).expect("history opens");
        if history.is_none() {
            println!("git not installed; skipping history test");
        }
        history
    }

    #[test]
    fn snapshot_versions_read_round_trip() {
        let data = tempfile::tempdir().expect("temp dir");
        let files = tempfile::tempdir().expect("temp dir");
        if let Some(history) = open(data.path()) {
            let file = files.path().join("notes.txt");
            assert!(history.versions(&file).expect("versions of unknown file").is_empty());
            std::fs::write(&file, b"one\r\n").expect("write");
            let v1 = history.snapshot(&file).expect("snapshot").expect("first snapshot stored");
            assert_eq!(v1.size, 5);
            assert_eq!(history.snapshot(&file).expect("snapshot"), None, "unchanged content is skipped");
            std::fs::write(&file, b"two\nlines\n").expect("write");
            let v2 = history.snapshot(&file).expect("snapshot").expect("changed content stored");
            let versions = history.versions(&file).expect("versions");
            assert_eq!(versions, vec![v2.clone(), v1.clone()]);
            // CRLF must survive regardless of the user's core.autocrlf.
            assert_eq!(history.read(&v1).expect("read v1"), b"one\r\n");
            assert_eq!(history.read(&v2).expect("read v2"), b"two\nlines\n");
            std::fs::write(&file, b"one\r\n").expect("write");
            let v3 = history.snapshot(&file).expect("snapshot").expect("reverting is a new version");
            assert_eq!(v3.blob, v1.blob);
            assert_eq!(history.versions(&file).expect("versions").len(), 3);
            // Reopening an existing repository keeps the data.
            let reopened = open(data.path()).expect("git still installed");
            assert_eq!(reopened.versions(&file).expect("versions").len(), 3);
        }
    }

    #[test]
    fn rename_moves_history() {
        let data = tempfile::tempdir().expect("temp dir");
        let files = tempfile::tempdir().expect("temp dir");
        if let Some(history) = open(data.path()) {
            let old = files.path().join("old name.md");
            let new = files.path().join("new name.md");
            std::fs::write(&old, "a").expect("write");
            history.snapshot(&old).expect("snapshot").expect("stored");
            std::fs::write(&old, "ab").expect("write");
            history.snapshot(&old).expect("snapshot").expect("stored");
            std::fs::rename(&old, &new).expect("rename on disk");
            history.rename(&old, &new).expect("rename history");
            assert!(history.versions(&old).expect("versions").is_empty());
            let moved = history.versions(&new).expect("versions");
            assert_eq!(moved.len(), 2);
            assert_eq!(history.read(&moved[0]).expect("read"), b"ab");
            assert_eq!(history.snapshot(&new).expect("snapshot"), None, "history continues under the new name");
            history.rename(&files.path().join("never.txt"), &new).expect("renaming unknown file is a no-op");
            assert_eq!(history.versions(&new).expect("versions").len(), 2);
        }
    }

    #[test]
    fn distinct_files_have_distinct_histories() {
        let data = tempfile::tempdir().expect("temp dir");
        let files = tempfile::tempdir().expect("temp dir");
        if let Some(history) = open(data.path()) {
            let a = files.path().join("a.txt");
            let b = files.path().join("b.txt");
            std::fs::write(&a, "same").expect("write");
            std::fs::write(&b, "same").expect("write");
            assert!(history.snapshot(&a).expect("snapshot").is_some());
            assert!(history.snapshot(&b).expect("snapshot").is_some());
            assert_eq!(history.versions(&a).expect("versions").len(), 1);
            assert!(matches!(history.snapshot(&files.path().join("missing.txt")), Err(ToolError::Failed(_))));
        }
    }
}
