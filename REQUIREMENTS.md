# Kladde – Requirements

Lightweight text and hex editor with workspaces for Windows 11 and Linux.

Priority: **M** = first release, **2** = second iteration, **L** = later. All items except X11 are in scope.

## Decisions

| Topic | Decision |
|---|---|
| Stack | Rust, egui/eframe 0.29, single native binary |
| Name | Kladde; all workspaces in one JSON file `workspaces.json` in the data directory |
| Licence | MIT |
| External tools | git, quarto, interpreters are optional; features hide when missing |

## Workspaces
| ID | Requirement | Prio |
|---|---|---|
| W1 | All workspaces live in one fixed JSON file in the data directory, saved automatically; each holds a tree of groups and files. Paths are absolute, relative to the data directory in portable mode | M |
| W2 | Node types: virtual group (any files) and linked folder (mirrors disk, include/exclude globs) | M |
| W3 | Fold/unfold nodes; state persisted | M |
| W4 | Color per node (palette + custom); tabs inherit the color of their node | M |
| W5 | Rename: display alias (workspace only) and separate "rename on disk" | M |
| W6 | Drag & drop reorder and move between groups | M |
| W7 | Per-workspace session: open tabs, cursors, layout | M |
| W8 | Several workspaces; the strip below the menu bar shows each as a tab group (folded: name only; unfolded: name followed by its file tabs); quick switcher; create, rename, delete | M |
| W9 | Mark missing files, offer relocate | 2 |
| W10 | Fuzzy file open within workspace (Ctrl+P) | M |

## Text editor
| ID | Requirement | Prio |
|---|---|---|
| E1 | Tabs; session restore including unsaved buffers (hot exit) | M |
| E2 | Syntax highlighting: C/C++, Python, Rust, JSON, XML/HTML, Markdown/Quarto, INI/TOML, YAML, shell, PowerShell, batch, JS, CSV | M |
| E3 | Encodings: detect/convert UTF-8/16 with or without BOM, Windows-1252, ISO-8859-x | M |
| E4 | Line endings: detect/convert CRLF/LF/CR; show whitespace | M |
| E5 | Find/replace: regex, current file and workspace; results panel | M |
| E6 | Column selection (Alt+drag) and multiple cursors | M |
| E7 | Code folding, bookmarks, go to line / byte offset | 2 |
| E8 | Reload prompt on external change; tail -f mode for logs | M |
| E9 | Split view, same file in both panes | 2 |
| E10 | Compare two files (diff) | L |
| E11 | Macro record/replay | L |

## Hex / binary
| ID | Requirement | Prio |
|---|---|---|
| H1 | Offset, hex and ASCII columns; bytes per row and grouping 1/2/4/8 configurable | M |
| H2 | Binary (bit) view as alternative to hex | M |
| H3 | Auto-detect binary files; switch any file between text and hex | M |
| H4 | Selection synchronised between hex and ASCII column | M |
| H5 | Data inspector: int8–64 signed/unsigned LE/BE, float32/64, UTF-8 char, Unix time | 2 |
| H6 | Overwrite editing, search for byte patterns and text, go to offset | M |
| H7 | Insert/delete bytes | 2 |
| H8 | Text column encoding: ASCII, Latin-1, UTF-8 | 2 |

## Large files (>10 GB)
| ID | Requirement | Prio |
|---|---|---|
| L1 | Opens quickly regardless of size; content read on demand, never fully loaded | M |
| L2 | Edits stored as changes over the original (piece table) | M |
| L3 | Line index built in background with progress; scrollbar approximate until done | M |
| L4 | Search runs in background, cancellable, with progress | M |
| L5 | Save writes a temporary file, then replaces the original | M |
| L6 | Above a configurable size, highlighting and word wrap switch off | M |
| L7 | Very long lines (e.g. 1 GB minified JSON) do not freeze the editor | M |
| L8 | Read-only mode that opens files without locking them | M |

## Code block execution
| ID | Requirement | Prio |
|---|---|---|
| R1 | Fenced blocks (```` ```powershell ````, `pwsh`, `cmd`, `bash`, `sh`, `python`, `node`, `r`) in any file show a ▶ run button; output streams into the Output panel; working dir = file dir | M |
| R2 | "Run in terminal": opens an external terminal (PowerShell window on Windows) that runs the block and stays open | M |
| R3 | Run the selection with a chosen interpreter; run the whole file by extension (`.ps1`, `.py`, `.sh`, `.bat`, `.cmd`, `.js`, `.R`) | M |
| R4 | Stop a running process; exit code shown | M |

## Automatic file versions
| ID | Requirement | Prio |
|---|---|---|
| V1 | Every save stores a version in a background git repository in the app data dir; user folders untouched | M |
| V2 | Version list per file (time, size): view, diff against current, restore (undoable) | M |
| V3 | Size limit (default 50 MB) and on/off switch | M |
| V4 | Versions follow files renamed on disk through Kladde | 2 |

## Quarto
| ID | Requirement | Prio |
|---|---|---|
| Q1 | Detect `quarto` on PATH; "Render" for `.qmd`, `.md`, `.ipynb`, `.Rmd` with format choice (default/html/pdf/docx) | M |
| Q2 | Render output streams into the Output panel; result opens when done | M |
| Q3 | Quarto actions disabled with a hint when quarto is missing | M |

## Platform and weight
| ID | Requirement | Prio |
|---|---|---|
| P1 | Windows 11 and Linux (X11 and Wayland); one executable, no installer | M |
| P2 | Targets: <15 MB on disk, <200 ms start, <50 MB idle RAM | M |
| P3 | Dark/light theme following the OS; sharp on high-DPI | M |
| P4 | Portable mode: settings next to the executable | M |
| P5 | No telemetry; network access only for the update check (U1) | M |

## Updates
| ID | Requirement | Prio |
|---|---|---|
| U1 | Help > Check for updates asks the latest GitHub release; optional check at every start, off by default | M |
| U2 | Install downloads the platform binary, verifies its SHA-256, replaces the executable and restarts after saving the session | M |
| U3 | Releases built and published by GitHub Actions from a `v<version>` tag, with `latest.json` | M |

## Convenience
| ID | Requirement | Prio |
|---|---|---|
| X1 | Command palette (Ctrl+Shift+P) | M |
| X2 | Notepad++-compatible default shortcuts, remappable | M |
| X3 | Line tools: sort, remove duplicates, trim, case, join/split | 2 |
| X4 | Pretty-print and minify JSON/XML | 2 |
| X5 | Base64, URL and hex encode/decode of selection | 2 |
| X6 | MD5/SHA-1/SHA-256/CRC32 of selection or whole file (streamed) | 2 |
| X7 | Automatic backup snapshots every N seconds | M |
| X8 | Recent files; open by drag & drop | M |
| X9 | Git status markers in the workspace tree (read-only) | L |
| X10 | CSV as table | L |
| X11 | Plugin system | out of scope (lightweight) |
