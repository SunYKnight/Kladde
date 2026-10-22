//! Dev harness: a workspace in a temp dir with groups, files, a linked folder, a missing file and colors.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui;
use kladde_tools::GitStatus;
use kladde_workspace::{Workspace, PALETTE};
use kladde_wstree::{TreeContext, TreeEvent, TreeState};

struct Demo {
    _dir: tempfile::TempDir,
    ws: Workspace,
    state: TreeState,
    open: HashSet<PathBuf>,
    dirty: HashSet<PathBuf>,
    git: HashMap<PathBuf, GitStatus>,
    active: Option<PathBuf>,
    dark: bool,
    log: Vec<String>,
}

fn build(dir: &Path) -> Result<Workspace, Box<dyn Error>> {
    let project = dir.join("project");
    fs::create_dir_all(project.join("src"))?;
    fs::create_dir_all(project.join("docs"))?;
    let files = [
        ("README.md", "# Demo\n"),
        ("notes.txt", "notes\n"),
        ("build.ps1", "Write-Output hi\n"),
        ("run.py", "print('hi')\n"),
        ("project/Cargo.toml", "[package]\n"),
        ("project/cache.tmp", "excluded\n"),
        ("project/src/main.rs", "fn main() {}\n"),
        ("project/docs/guide.qmd", "# Guide\n"),
    ];
    files.iter().try_for_each(|(name, text)| fs::write(dir.join(name), text))?;
    let mut ws = Workspace::new("Demo");
    let root = ws.root.id;
    let docs = ws.add_group(root, "Docs")?;
    ws.set_color(docs, Some(PALETTE[0]));
    ws.add_file(docs, &dir.join("README.md"))?;
    let notes = ws.add_file(docs, &dir.join("notes.txt"))?;
    ws.set_alias(notes, Some("My notes".into()));
    let scripts = ws.add_group(root, "Scripts")?;
    ws.set_color(scripts, Some(PALETTE[4]));
    ws.add_file(scripts, &dir.join("build.ps1"))?;
    let run = ws.add_file(scripts, &dir.join("run.py"))?;
    ws.set_color(run, Some(PALETTE[8]));
    ws.set_expanded(scripts, false);
    ws.add_file(root, &dir.join("deleted.txt"))?;
    let linked = ws.add_folder(root, &project, vec![], vec!["*.tmp".into()])?;
    ws.set_color(linked, Some(PALETTE[6]));
    ws.set_expanded(linked, true);
    Ok(ws)
}

impl Demo {
    fn new() -> Result<Demo, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let ws = build(dir.path())?;
        let notes = dir.path().join("notes.txt");
        let git = HashMap::from([
            (dir.path().join("README.md"), GitStatus::Modified),
            (dir.path().join("run.py"), GitStatus::Added),
            (dir.path().join("project").join("src"), GitStatus::Untracked),
        ]);
        Ok(Demo {
            open: HashSet::from([notes.clone()]),
            dirty: HashSet::from([notes.clone()]),
            active: Some(notes),
            _dir: dir,
            ws,
            state: TreeState::new(),
            git,
            dark: true,
            log: Vec::new(),
        })
    }

    fn handle(&mut self, event: TreeEvent) {
        if let TreeEvent::Open(path) = &event {
            self.open.insert(path.clone());
            self.active = Some(path.clone());
        }
        self.log.push(format!("{event:?}"));
    }
}

impl eframe::App for Demo {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.set_visuals(if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() });
        egui::SidePanel::left("tree").default_width(320.0).show(ctx, |ui| {
            let tree_ctx = TreeContext {
                active_file: self.active.as_deref(),
                open_files: &self.open,
                dirty_files: &self.dirty,
                git: Some(&self.git),
                quarto_available: true,
                dark: self.dark,
            };
            let events = kladde_wstree::show(ui, &mut self.ws, &mut self.state, &tree_ctx);
            events.into_iter().for_each(|e| self.handle(e));
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.dark, "Dark");
                if ui.button("Rename selected").clicked() {
                    self.state.start_rename_selected();
                }
                ui.label(if self.ws.is_dirty() { "workspace modified" } else { "workspace clean" });
            });
            ui.separator();
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                self.log.iter().for_each(|line| {
                    ui.monospace(line);
                });
            });
        });
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let demo = Demo::new()?;
    let options = eframe::NativeOptions::default();
    eframe::run_native("kladde-wstree demo", options, Box::new(|_cc| Ok(Box::new(demo))))?;
    Ok(())
}
