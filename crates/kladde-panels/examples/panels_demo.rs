//! Dev harness: `cargo run -p kladde-panels --example panels_demo [dir-or-file-to-search]`.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui;
use kladde_core::SearchPattern;
use kladde_panels::{
    commands, settings_capturing, show_csv, show_diff, show_find_bar, show_history, show_output, show_palette,
    show_results, show_settings, DiffTheme, FilesSearchJob, FindState, HistoryEvent, HistoryState, Keymap, OutputState,
    PaletteItem, PaletteState, ResultsState,
};
use kladde_tools::{diff_text, parse_csv, run_script, CsvTable, DiffLine, History, Interpreter};
use kladde_workspace::Settings;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Palette,
    Find,
    Results,
    Output,
    Diff,
    Csv,
    History,
    Settings,
}

const TABS: [(Tab, &str); 8] = [
    (Tab::Palette, "Palette"),
    (Tab::Find, "Find bar"),
    (Tab::Results, "Results"),
    (Tab::Output, "Output"),
    (Tab::Diff, "Diff"),
    (Tab::Csv, "CSV"),
    (Tab::History, "History"),
    (Tab::Settings, "Settings"),
];

struct Demo {
    tab: Tab,
    keymap: Keymap,
    palette: PaletteState,
    items: Vec<PaletteItem>,
    log: Vec<String>,
    find: FindState,
    results: ResultsState,
    files: Vec<PathBuf>,
    output: OutputState,
    diff: Vec<DiffLine>,
    csv: CsvTable,
    settings: Settings,
    settings_open: bool,
    history: HistoryState,
    /// Smoke mode (KLADDE_DEMO_CYCLE set): frames rendered while visiting every tab once, then exit.
    cycle: Option<usize>,
}

fn collect_files(path: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.is_dir() {
        fs::read_dir(path)?.try_for_each(|entry| collect_files(&entry?.path(), out))
    } else {
        out.push(path.to_path_buf());
        Ok(())
    }
}

fn bmp(img: &egui::ColorImage) -> Vec<u8> {
    let [w, h] = img.size;
    let row = (w * 3).div_ceil(4) * 4;
    let size = 54 + row * h;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0]);
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&[1, 0, 24, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&((row * h) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    img.pixels.chunks(w).for_each(|line| {
        line.iter().for_each(|p| out.extend_from_slice(&[p.b(), p.g(), p.r()]));
        out.resize(out.len() + row - w * 3, 0);
    });
    out
}

fn restore_len(e: &HistoryEvent) -> usize {
    match e {
        HistoryEvent::Restore { bytes, .. } => bytes.len(),
    }
}

fn palette_items(keymap: &Keymap) -> Vec<PaletteItem> {
    commands()
        .iter()
        .map(|c| PaletteItem {
            id: c.id.to_string(),
            label: format!("{}: {}", c.category, c.label),
            detail: keymap.chord_for(c.id).map(|ch| ch.to_string()).unwrap_or_default(),
        })
        .collect()
}

impl Demo {
    fn new(files: Vec<PathBuf>) -> Result<Demo, String> {
        let settings = Settings::default();
        let keymap = Keymap::new(&settings.keybindings);
        let csv = parse_csv("name;size;kind\nlib.rs;1200;rust\ndemo.rs;6400;rust\nREADME.md;300;markdown\n", 1000)?;
        let old = "fn main() {\n    println!(\"hello\");\n}\n";
        let new = "fn main() {\n    let who = \"world\";\n    println!(\"hello {who}\");\n}\n";
        let mut demo = Demo {
            tab: Tab::Palette,
            items: palette_items(&keymap),
            keymap,
            palette: PaletteState::new(),
            log: Vec::new(),
            find: FindState::new(),
            results: ResultsState::new(),
            files,
            output: OutputState::new(),
            diff: diff_text(old, new),
            csv,
            settings,
            settings_open: false,
            history: HistoryState::new(),
            cycle: std::env::var_os("KLADDE_DEMO_CYCLE").map(|_| 0),
        };
        demo.find.open(true, "fn ");
        demo.start_search();
        demo.run_echo();
        if let Err(e) = demo.load_history() {
            demo.log.push(format!("history: {e}"));
        }
        Ok(demo)
    }

    fn start_search(&mut self) {
        match self.find.pattern() {
            Ok(p) => {
                let job = FilesSearchJob::start(self.files.clone(), p, 10_000);
                self.results.set_job(format!("\"{}\"", self.find.query), job);
            }
            Err(e) => self.log.push(format!("search: {e}")),
        }
    }

    /// Writes screenshots to KLADDE_DEMO_SHOTS as BMP files (no image crate needed).
    fn save_screenshots(&mut self, ctx: &egui::Context) {
        let dir = std::env::var_os("KLADDE_DEMO_SHOTS").map(PathBuf::from);
        let shots: Vec<std::sync::Arc<egui::ColorImage>> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
                .collect()
        });
        if let Some(dir) = dir {
            shots.iter().for_each(|img| {
                let n = self.log.iter().filter(|l| l.starts_with("screenshot")).count();
                let path = dir.join(format!("shot{n}.bmp"));
                match fs::write(&path, bmp(img)) {
                    Ok(()) => self.log.push(format!("screenshot {}", path.display())),
                    Err(e) => self.log.push(format!("screenshot failed: {e}")),
                }
            });
        }
    }

    fn palette_or_settings(&mut self, tab: Tab, first_frame: bool) {
        if first_frame && tab == Tab::Palette {
            self.palette.open("Command");
        }
        if first_frame && tab != Tab::Palette {
            self.palette.close();
        }
        if first_frame && tab == Tab::Settings {
            self.settings_open = true;
        }
    }

    /// Two versions of a scratch file in a scratch history repository.
    fn load_history(&mut self) -> Result<(), Box<dyn Error>> {
        let dir = std::env::temp_dir().join("kladde-panels-demo");
        fs::create_dir_all(&dir)?;
        let file = dir.join("notes.txt");
        let history = History::open(&dir)?.ok_or("git is not installed")?;
        fs::write(&file, "first line\nsecond line\n")?;
        history.snapshot(&file)?;
        fs::write(&file, "first line\nsecond line, edited\nthird line\n")?;
        history.snapshot(&file)?;
        self.history.show_file(Arc::new(history), file);
        Ok(())
    }

    fn run_echo(&mut self) {
        let (interp, code) = if cfg!(windows) {
            (Interpreter::PowerShell, "echo hi\n[Console]::Error.WriteLine('to-stderr')\nexit 3")
        } else {
            (Interpreter::Bash, "echo hi\necho to-stderr 1>&2\nexit 3")
        };
        let started = std::env::current_dir()
            .map_err(|e| e.to_string())
            .and_then(|cwd| run_script(interp, code, &cwd).map_err(|e| e.to_string()));
        match started {
            Ok(handle) => self.output.add_run(handle, None),
            Err(e) => self.output.add_message("echo failed", &e),
        }
    }
}

impl eframe::App for Demo {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(frame) = self.cycle {
            const FRAMES_PER_TAB: usize = 30;
            match TABS.get(frame / FRAMES_PER_TAB) {
                Some((tab, _)) => {
                    self.tab = *tab;
                    self.palette_or_settings(*tab, frame % FRAMES_PER_TAB == 0);
                    if frame % FRAMES_PER_TAB == FRAMES_PER_TAB - 5 {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                    }
                }
                None => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
            self.cycle = Some(frame + 1);
            self.save_screenshots(ctx);
            ctx.request_repaint();
        }
        if !settings_capturing(ctx) && !self.palette.is_open() {
            self.keymap.take_triggered(ctx).into_iter().for_each(|id| {
                self.log.push(format!("shortcut: {id}"));
                match id {
                    "palette.commands" => self.palette.open("Command"),
                    "settings.open" => self.settings_open = true,
                    "search.find" => self.find.open(false, ""),
                    _ => {}
                }
            });
        }
        let output_events = self.output.poll();
        output_events.iter().for_each(|e| self.log.push(format!("output: {e:?}")));
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                TABS.iter().for_each(|(t, label)| {
                    ui.selectable_value(&mut self.tab, *t, *label);
                });
            });
        });
        egui::TopBottomPanel::bottom("log").resizable(true).default_height(90.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                self.log.iter().for_each(|l| {
                    ui.monospace(l);
                });
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Palette => {
                ui.label("Press Ctrl+Shift+P (or the button) for the command palette. Other shortcuts are logged.");
                if ui.button("Open palette").clicked() {
                    self.palette.open("Command");
                }
            }
            Tab::Find => {
                if !self.find.is_open() && ui.button("Open find bar").clicked() {
                    self.find.open(false, "");
                }
                if self.find.is_open() {
                    let status = match self.find.pattern() {
                        Ok(SearchPattern::Bytes(b)) => format!("{} bytes", b.len()),
                        Ok(SearchPattern::Text { .. }) => "text".to_string(),
                        Err(e) => e,
                    };
                    let actions = show_find_bar(ui, &mut self.find, &status);
                    if actions.contains(&kladde_panels::FindAction::FindAll) {
                        self.start_search();
                        self.tab = Tab::Results;
                    }
                    actions.iter().for_each(|a| self.log.push(format!("find: {a:?}")));
                }
            }
            Tab::Results => {
                let events = show_results(ui, &mut self.results);
                events.iter().for_each(|e| self.log.push(format!("results: {e:?}")));
            }
            Tab::Output => {
                if ui.button("Run echo again").clicked() {
                    self.run_echo();
                }
                let events = show_output(ui, &mut self.output);
                events.iter().for_each(|e| self.log.push(format!("output: {e:?}")));
            }
            Tab::Diff => {
                let theme = if ui.visuals().dark_mode { DiffTheme::dark() } else { DiffTheme::light() };
                show_diff(ui, &self.diff, &theme);
            }
            Tab::Csv => show_csv(ui, &self.csv),
            Tab::History => {
                let events = show_history(ui, &mut self.history, Some("first line\nthird line\n"));
                events.iter().for_each(|e| self.log.push(format!("history: restore {} bytes", restore_len(e))));
            }
            Tab::Settings => {
                if ui.button("Open settings").clicked() {
                    self.settings_open = true;
                }
                ui.label(format!("Overrides: {:?}", self.settings.keybindings));
            }
        });
        if let Some(id) = show_palette(ctx, &mut self.palette, &self.items) {
            self.log.push(format!("palette: {id}"));
        }
        if show_settings(ctx, &mut self.settings_open, &mut self.settings) {
            self.keymap = Keymap::new(&self.settings.keybindings);
            self.items = palette_items(&self.keymap);
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let root =
        std::env::args_os().nth(1).map_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), PathBuf::from);
    let mut files = Vec::new();
    collect_files(&root, &mut files)?;
    let demo = Demo::new(files)?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1000.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native("kladde-panels demo", options, Box::new(|_cc| Ok(Box::new(demo))))?;
    Ok(())
}
