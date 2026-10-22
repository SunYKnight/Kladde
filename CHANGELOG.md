# Changelog

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
