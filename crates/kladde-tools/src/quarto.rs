use std::path::{Path, PathBuf};

use crate::runner::{spawn, RunHandle};
use crate::{find_program, ToolError};

pub fn quarto_path() -> Option<PathBuf> {
    find_program("quarto")
}

/// Documents Quarto can render: .qmd, .md, .ipynb, .Rmd.
pub fn can_render(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "qmd" | "md" | "ipynb" | "rmd"))
}

/// Starts `quarto render <file> [--to format]` in the document's folder; output streams through the handle.
pub fn quarto_render(path: &Path, format: Option<&str>) -> Result<RunHandle, ToolError> {
    let quarto = quarto_path().ok_or_else(|| ToolError::NotFound("quarto".to_string()))?;
    let name = path.file_name().ok_or_else(|| ToolError::Failed(format!("not a file: {}", path.display())))?;
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut cmd = std::process::Command::new(quarto);
    cmd.arg("render").arg(name).current_dir(dir);
    if let Some(format) = format {
        cmd.arg("--to").arg(format);
    }
    spawn(format!("quarto render {}", name.to_string_lossy()), cmd, None)
}

/// The rendered file from a render log ("Output created: X", relative to the document's folder).
pub fn quarto_output_file(doc: &Path, log: &str) -> Option<PathBuf> {
    let dir = doc.parent().unwrap_or(Path::new(""));
    log.lines()
        .rev()
        .find_map(|line| line.split_once("Output created:").map(|(_, rest)| rest.trim()).filter(|r| !r.is_empty()))
        .map(|rest| dir.join(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_render_by_extension() {
        assert!(can_render(Path::new("a/report.qmd")));
        assert!(can_render(Path::new("README.MD")));
        assert!(can_render(Path::new("nb.ipynb")));
        assert!(can_render(Path::new("x.Rmd")));
        assert!(!can_render(Path::new("x.txt")));
        assert!(!can_render(Path::new("qmd")));
    }

    #[test]
    fn output_file_relative_to_document() {
        let doc = Path::new("docs").join("report.qmd");
        let log = "processing file: report.qmd\n  1/2\npandoc\n\nOutput created: report.html\n\n";
        assert_eq!(quarto_output_file(&doc, log), Some(Path::new("docs").join("report.html")));
        let log = "Output created: _site/sub/report.pdf\r\n";
        assert_eq!(quarto_output_file(&doc, log), Some(Path::new("docs").join("_site/sub/report.pdf")));
        assert_eq!(quarto_output_file(&doc, "ERROR: render failed"), None);
    }

    #[test]
    fn render_produces_output_file() {
        if quarto_path().is_none() {
            println!("quarto not installed; skipping render test");
        } else {
            let dir = tempfile::tempdir().expect("temp dir");
            let doc = dir.path().join("hello.md");
            std::fs::write(&doc, "# Hello\n\nText.\n").expect("write doc");
            let handle = quarto_render(&doc, Some("html")).expect("render starts");
            let start = std::time::Instant::now();
            let mut log = String::new();
            let mut code = None;
            while code.is_none() {
                assert!(start.elapsed().as_secs() < 180, "render timed out: {log}");
                std::thread::sleep(std::time::Duration::from_millis(50));
                handle.poll().into_iter().for_each(|ev| match ev {
                    crate::RunEvent::Stdout(l) | crate::RunEvent::Stderr(l) => log.push_str(&(l + "\n")),
                    crate::RunEvent::Exited(c) => code = Some(c),
                });
            }
            assert_eq!(code, Some(Some(0)), "{log}");
            let out = quarto_output_file(&doc, &log).expect("log names the output");
            assert!(out.is_file(), "{out:?} missing; log: {log}");
        }
    }
}
