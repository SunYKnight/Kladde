# Kladde

Lightweight text and hex editor with workspaces for Windows 11 and Linux. Feature list: [REQUIREMENTS.md](REQUIREMENTS.md).

## Build

```sh
cargo build --release          # binary: target/release/kladde(.exe)
cargo test --workspace
```

`rust-toolchain.toml` pins the toolchain used for development; the code builds on stable Rust >= 1.83.
Linux needs the usual X11/Wayland development packages for eframe (`libxkbcommon-dev`, `libgl1-mesa-dev`).

## Run

```sh
kladde [FILE...] [WORKSPACE.kladde]
```

- Portable mode: create an empty file `kladde.portable` next to the executable; settings, backups and file
  history then live in `kladde-data/` beside it. Otherwise they live in `%APPDATA%\Kladde` (Windows) or
  `~/.local/share/Kladde` (Linux).
- Optional external tools: `git` (automatic file versions, git status in the tree), `quarto` (render),
  PowerShell / Git Bash / Python / Node / R (running fenced code blocks).

## Layout

| Crate | Purpose |
|---|---|
| `kladde-core` | Text engine: memory-mapped/paged sources, piece table, undo, encodings, line index, search, save |
| `kladde-workspace` | Workspace tree model, session, settings, fuzzy matching |
| `kladde-tools` | Process runner, Quarto, git-backed file history, git status, text transforms, hashes, diff, CSV |
| `kladde-syntax` | Lightweight lexers for highlighting and folding |
| `kladde-editor` | egui text view widget |
| `kladde-hex` | egui hex/binary view and data inspector |
| `kladde-wstree` | egui workspace tree panel |
| `kladde-panels` | Commands/keymap, palette, find, results, output, history, diff, CSV, settings |
| `kladde` | The application |
