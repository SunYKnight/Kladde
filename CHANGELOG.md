# Changelog

## 0.1.4

### Added

- Hex view: bytes per row can be switched from the status bar (`16/row`): 4, 8, 16, 24, 32, 48 or 64. Settings
  > Hex offers 4 as well.

### Changed

- Every pane shows the name and folder of its active file above the content, also when the workspace strip is
  folded.

### Fixed

- Hex view: the byte column header no longer drifts from the data rows at fractional display scaling (e.g. 125 %
  or 150 % on Windows).

## 0.1.3

### Added

- Dropping files or folders on the window asks whether to open them, add them to the active workspace, or both.
- Tab context menu: "Add to workspace".

### Changed

- Linux: Kladde runs through XWayland in Wayland sessions, where the window library has no file drag & drop.

### Fixed

- Adding a file or folder that is already in the workspace no longer creates a second entry.

## 0.1.2

### Added

- Installers for Windows (`kladde-x86_64-windows-setup.exe`: Start menu, "Open with", uninstall entry) and Linux
  (`kladde-x86_64-linux-installer.sh`: launcher and icon in the application menu, `--uninstall`). Both install for
  the current user, so in-app updates keep working; the standalone binaries remain.

## 0.1.1

### Changed

- New application icon, also embedded in the Windows executable.

## 0.1.0

First public release.

### Added

- Text and hex editor with workspaces, large-file support, code block execution, automatic file versions and Quarto
  rendering (see [REQUIREMENTS.md](REQUIREMENTS.md)).
- Workspace strip below the menu bar: every workspace is a tab group, folded to its name or unfolded to its file
  tabs. All workspaces are kept in one file, `workspaces.json` in the data folder, and saved automatically.
- Help > Check for updates installs a newer release from GitHub and restarts; Settings > Updates can check at every
  start (off by default).
