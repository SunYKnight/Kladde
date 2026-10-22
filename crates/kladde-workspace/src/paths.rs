use std::path::{Component, Path, PathBuf};

/// Absolute, lexically normalized form of `path` (no `.`/`..`, no symlink resolution).
pub(crate) fn absolute(path: &Path) -> std::io::Result<PathBuf> {
    Ok(normalize(&std::path::absolute(path)?))
}

/// Removes `.` and resolves `..` lexically.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    path.components().fold(PathBuf::new(), |mut acc, c| {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(acc.components().next_back(), Some(Component::Normal(_))) {
                    acc.pop();
                } else if !acc.has_root() {
                    acc.push("..");
                }
            }
            other => acc.push(other.as_os_str()),
        }
        acc
    })
}

/// Windows file systems are case-insensitive; matching must follow that.
fn component_eq(a: Component, b: Component) -> bool {
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// Remainder of `path` below `base` if `base` is `path` or one of its ancestors.
pub(crate) fn strip_base(path: &Path, base: &Path) -> Option<PathBuf> {
    let mut rest = path.components();
    let matched = base.components().all(|bc| rest.next().is_some_and(|c| component_eq(c, bc)));
    matched.then(|| rest.as_path().to_path_buf())
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    strip_base(a, b).is_some_and(|rest| rest.as_os_str().is_empty())
}

/// String form with `/` separators so workspace files are identical across platforms.
fn slashed(path: &Path) -> String {
    let s = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s
    }
}

/// Form stored in workspace files: relative to `base` when both share a root, else absolute.
pub(crate) fn to_stored(path: &Path, base: &Path) -> String {
    let p: Vec<Component> = path.components().collect();
    let b: Vec<Component> = base.components().collect();
    let roots = p.iter().take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir)).count();
    let common = p.iter().zip(&b).take_while(|(x, y)| component_eq(**x, **y)).count();
    if path.is_absolute() && roots > 0 && common >= roots {
        let ups = std::iter::repeat_n("..".to_string(), b.len() - common);
        let downs = p[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned());
        let parts: Vec<String> = ups.chain(downs).collect();
        if parts.is_empty() {
            ".".to_string()
        } else {
            parts.join("/")
        }
    } else {
        slashed(path)
    }
}

/// Inverse of [`to_stored`].
pub(crate) fn from_stored(stored: &Path, base: &Path) -> PathBuf {
    let text = stored.to_string_lossy();
    let native = if cfg!(windows) { text.replace('/', "\\") } else { text.into_owned() };
    let p = PathBuf::from(native);
    normalize(&if p.is_absolute() { p } else { base.join(p) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_round_trip_covers_parent_and_child_paths() {
        let base = std::env::temp_dir().join("ws").join("proj");
        let cases = [base.join("a").join("b.txt"), base.join("..").join("other").join("c.txt"), base.clone()];
        let expected = ["a/b.txt", "../other/c.txt", "."];
        cases.iter().zip(expected).for_each(|(case, want)| {
            let abs = normalize(case);
            let stored = to_stored(&abs, &base);
            assert_eq!(stored, want);
            assert!(same_path(&from_stored(Path::new(&stored), &base), &abs));
        });
    }

    #[cfg(windows)]
    #[test]
    fn other_drive_stays_absolute() {
        let stored = to_stored(Path::new(r"D:\data\x.txt"), Path::new(r"C:\proj"));
        assert_eq!(stored, "D:/data/x.txt");
        assert_eq!(from_stored(Path::new(&stored), Path::new(r"C:\proj")), PathBuf::from(r"D:\data\x.txt"));
    }
}
