# Kladde

Lightweight text and hex editor with workspaces for Windows 11 and Linux. Feature list: [REQUIREMENTS.md](REQUIREMENTS.md).
Licence: [MIT](LICENSE).

## Install

Download from the [latest release](https://github.com/SunYKnight/Kladde/releases/latest) and run it; there is no
installer:

- Windows: `kladde-x86_64-windows.exe`
- Linux (x86_64): `kladde-x86_64-linux`, made executable once with `chmod +x kladde-x86_64-linux`

Help > Check for updates downloads a newer release, replaces the program file and restarts Kladde. Settings > Updates
can check at every start; it is off by default, and Kladde contacts no server otherwise. The program file's folder
must be writable for updates (on Linux, keep it e.g. in `~/.local/bin`, not `/usr/local/bin`).

## Build

```sh
cargo build --release          # binary: target/release/kladde(.exe)
cargo test --workspace
```

`rust-toolchain.toml` pins the toolchain used for development; the code builds on stable Rust >= 1.85.
Linux needs the usual X11/Wayland development packages for eframe (`libxkbcommon-dev`, `libgl1-mesa-dev`).

## Run

```sh
kladde [FILE...]
```

- Workspaces: the strip below the menu bar shows every workspace as a tab group. A folded workspace shows only its
  name; an unfolded one shows its file tabs after it. Click a folded workspace to switch to it, click the active one
  to fold or unfold it, right-click to rename or delete, `+` creates one. All workspaces, their trees and tab lists
  are saved automatically to `workspaces.json` in the data folder.
- Portable mode: create an empty file `kladde.portable` next to the executable; settings, workspaces, backups and
  file history then live in `kladde-data/` beside it, and workspace paths are stored relative to that folder so a
  changed drive letter does not break them. Otherwise they live in `%APPDATA%\Kladde` (Windows) or
  `~/.local/share/Kladde` (Linux).
- Optional external tools: `git` (automatic file versions, git status in the tree), `quarto` (render),
  PowerShell / Git Bash / Python / Node / R (running fenced code blocks).

## Release

1. Set the new version in the root `Cargo.toml` under `[workspace.package]`, e.g. `version = "0.2.0"`, and run
   `cargo build` so `Cargo.lock` follows.
2. In `CHANGELOG.md`, rename `## Unreleased` to `## 0.2.0`; it must be the newest `## ` heading. Its text becomes
   the release notes the update dialog shows.
3. Commit, push to `master` and wait for CI.
4. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.
5. The _Release_ workflow (`.github/workflows/release.yml`) checks that tag, `Cargo.toml` and `CHANGELOG.md` agree,
   builds `kladde-x86_64-linux` and `kladde-x86_64-windows.exe`, and publishes the GitHub release with
   `latest.json` (version, notes, and per platform the download URL and SHA-256). Running copies find it through
   `https://github.com/SunYKnight/Kladde/releases/latest/download/latest.json`.

_Actions > Release > Run workflow_ is a dry run: it builds everything and uploads the binaries and `latest.json` as
workflow artifacts without publishing.

The updater checks the SHA-256 from `latest.json` against the download, which catches broken downloads; it does not
sign releases, so it trusts whoever can publish releases in the GitHub repository. To test an update without
GitHub, serve a `latest.json` and binary on `http://127.0.0.1:<port>/` and start Kladde with
`KLADDE_UPDATE_MANIFEST=http://127.0.0.1:<port>/latest.json`.

## Layout

| Crate | Purpose |
|---|---|
| `kladde-core` | Text engine: memory-mapped/paged sources, piece table, undo, encodings, line index, search, save |
| `kladde-workspace` | Workspace tree model, workspace store, session, settings, fuzzy matching |
| `kladde-tools` | Process runner, Quarto, git-backed file history, git status, text transforms, hashes, diff, CSV |
| `kladde-syntax` | Lightweight lexers for highlighting and folding |
| `kladde-editor` | egui text view widget |
| `kladde-hex` | egui hex/binary view and data inspector |
| `kladde-wstree` | egui workspace tree panel |
| `kladde-panels` | Commands/keymap, palette, find, results, output, history, diff, CSV, settings |
| `kladde-update` | Update check against GitHub releases, download, SHA-256 check, executable replacement |
| `kladde` | The application |
