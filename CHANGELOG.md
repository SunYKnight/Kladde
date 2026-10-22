# Changelog

## Unreleased

First public release.

### Added

- Text and hex editor with workspaces, large-file support, code block execution, automatic file versions and Quarto
  rendering (see [REQUIREMENTS.md](REQUIREMENTS.md)).
- Workspace strip below the menu bar: every workspace is a tab group, folded to its name or unfolded to its file
  tabs. All workspaces are kept in one file, `workspaces.json` in the data folder, and saved automatically.
- Help > Check for updates installs a newer release from GitHub and restarts; Settings > Updates can check at every
  start (off by default).
