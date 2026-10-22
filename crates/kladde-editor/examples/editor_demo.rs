//! Dev harness: `cargo run -p kladde-editor --example editor_demo [file] [--wrap] [--ws] [--light] [--fold-all]
//! [--showcase]`; `--showcase` adds a selection, extra carets, find matches and a bookmark.

use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use kladde_core::{Document, OpenOptions, Progress};
use kladde_editor::{EditorConfig, EditorEvent, EditorState, EditorTheme, Selection};

const SAMPLE: &str = r#"# Kladde editor demo

Some *emphasis*, a [link](https://example.com) and `inline code`.

## Script

```powershell
Get-ChildItem -Path . | Select-Object -First 5
Write-Host "Hello from PowerShell"
```

```python
def greet(name):
    print(f"hello {name}")
```

## List

- one
- two
	- tab indented
"#;

struct Demo {
    doc: Document,
    state: EditorState,
    cfg: EditorConfig,
    dark: bool,
    log: Vec<String>,
    frame_ms: f32,
}

impl eframe::App for Demo {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let started = Instant::now();
        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.cfg.word_wrap, "Wrap");
                ui.checkbox(&mut self.cfg.show_whitespace, "Whitespace");
                ui.checkbox(&mut self.cfg.show_line_numbers, "Line numbers");
                ui.checkbox(&mut self.dark, "Dark");
                if ui.button("Fold all").clicked() {
                    self.state.fold_all();
                }
                if ui.button("Unfold all").clicked() {
                    self.state.unfold_all();
                }
                if ui.button("Toggle comment").clicked() {
                    if let Err(e) = self.state.toggle_comment(&mut self.doc) {
                        self.log.push(e.to_string());
                    }
                }
                if ui.button("Bookmark").clicked() {
                    self.state.toggle_bookmark(&self.doc);
                }
                if ui.button("Next bookmark").clicked() {
                    self.state.next_bookmark(&self.doc);
                }
            });
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let (line, col) = self.state.caret_line_col(&self.doc);
            let line = line.map_or("?".to_string(), |l| (l + 1).to_string());
            let index = self.doc.line_index_progress().map_or(String::new(), |p| format!(" | index {:.0}%", p * 100.0));
            ui.label(format!(
                "Ln {line}, Col {} | {} bytes | {} | {} cursors | frame {:.2} ms{index} | {}",
                col + 1,
                self.doc.len(),
                self.state.language().name,
                self.state.selections().len(),
                self.frame_ms,
                self.log.last().map_or("", String::as_str),
            ));
        });
        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
            let theme = if self.dark { EditorTheme::dark() } else { EditorTheme::light() };
            let out =
                kladde_editor::show(ui, egui::Id::new("editor"), &mut self.doc, &mut self.state, &self.cfg, &theme);
            out.events.into_iter().for_each(|e| {
                self.log.push(match e {
                    EditorEvent::Run { interp, code, in_terminal } => {
                        format!("Run {} ({} bytes, terminal: {in_terminal})", interp.label(), code.len())
                    }
                    EditorEvent::Error(msg) => format!("Error: {msg}"),
                })
            });
        });
        self.frame_ms = started.elapsed().as_secs_f32() * 1000.0;
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let path = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from);
    let doc = match &path {
        Some(p) => Document::open(p, &OpenOptions::default(), &Progress::default())?,
        None => Document::from_bytes(SAMPLE.as_bytes().to_vec()),
    };
    let lang_path = path.clone().unwrap_or_else(|| PathBuf::from("demo.md"));
    let mut state = EditorState::new(Some(&lang_path));
    if flag("--fold-all") {
        state.fold_all();
    }
    if flag("--showcase") {
        let text = doc.read_string_lossy(0..doc.len().min(1 << 20));
        let matches: Vec<std::ops::Range<u64>> =
            text.match_indices("name").map(|(i, m)| i as u64..(i + m.len()) as u64).collect();
        state.set_find_matches(matches);
        state.set_caret(&doc, 40);
        state.toggle_bookmark(&doc);
        state.set_selections(&doc, vec![Selection { anchor: 2, caret: 8 }, Selection::caret(60), Selection::caret(90)]);
    }
    let cfg = EditorConfig { word_wrap: flag("--wrap"), show_whitespace: flag("--ws"), ..EditorConfig::default() };
    let demo = Demo { doc, state, cfg, dark: !flag("--light"), log: Vec::new(), frame_ms: 0.0 };
    let options = eframe::NativeOptions::default();
    eframe::run_native("Kladde editor demo", options, Box::new(|_| Ok(Box::new(demo))))?;
    Ok(())
}
