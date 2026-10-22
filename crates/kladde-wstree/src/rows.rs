use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kladde_workspace::{fuzzy_score, list_dir, DirEntry, Node, NodeId, NodeKind, Rgb, Workspace};

use crate::TreeEvent;

/// Cached one-level listing of a directory inside a linked folder; the error text is shown in the tree.
pub(crate) type Listing = Arc<Result<Vec<DirEntry>, String>>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RowKey {
    Node(NodeId),
    Entry(PathBuf),
    /// Listing error shown below the directory it belongs to.
    Note(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowKind {
    Group,
    File,
    Folder,
    Dir,
    EntryFile,
    Note,
}

impl RowKind {
    pub(crate) fn expandable(self) -> bool {
        matches!(self, RowKind::Group | RowKind::Folder | RowKind::Dir)
    }

    pub(crate) fn is_file(self) -> bool {
        matches!(self, RowKind::File | RowKind::EntryFile)
    }
}

/// One visible line of the tree, in pre-order.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub key: RowKey,
    /// Index of the parent row; always lower than the row's own index.
    pub parent: Option<usize>,
    pub depth: usize,
    pub label: String,
    pub kind: RowKind,
    pub path: Option<PathBuf>,
    pub expanded: bool,
    /// Effective color: own or inherited from the nearest coloured ancestor.
    pub color: Option<Rgb>,
    pub own_color: Option<Rgb>,
    pub missing: bool,
    /// Group that "New group" / "Add files…" target from this row.
    pub group: NodeId,
}

/// Inputs and outputs of one tree walk.
pub(crate) struct Lister<'a> {
    pub listings: &'a mut HashMap<PathBuf, Listing>,
    pub open_dirs: &'a HashSet<PathBuf>,
    pub missing: &'a HashSet<NodeId>,
    pub events: &'a mut Vec<TreeEvent>,
    /// Walk collapsed groups and cached listings too (filtering).
    pub all: bool,
    pub rows: Vec<Row>,
}

#[derive(Clone, Copy)]
struct Place {
    depth: usize,
    parent: Option<usize>,
    color: Option<Rgb>,
    group: NodeId,
}

impl Place {
    fn below(self, parent: usize, color: Option<Rgb>, group: NodeId) -> Place {
        Place { depth: self.depth + 1, parent: Some(parent), color, group }
    }
}

type Globs<'g> = (&'g [String], &'g [String]);

impl Lister<'_> {
    fn push(&mut self, row: Row) -> usize {
        self.rows.push(row);
        self.rows.len() - 1
    }

    fn node(&mut self, node: &Node, place: Place) {
        let color = node.color.or(place.color);
        let missing = self.missing.contains(&node.id);
        let (kind, group) = match node.kind {
            NodeKind::Group => (RowKind::Group, node.id),
            NodeKind::File { .. } => (RowKind::File, place.group),
            NodeKind::Folder { .. } => (RowKind::Folder, place.group),
        };
        let index = self.push(Row {
            key: RowKey::Node(node.id),
            parent: place.parent,
            depth: place.depth,
            label: node.display_name(),
            kind,
            path: node.path().map(Path::to_path_buf),
            expanded: node.expanded,
            color,
            own_color: node.color,
            missing,
            group,
        });
        let child = place.below(index, color, group);
        match &node.kind {
            NodeKind::Group if node.expanded || self.all => node.children.iter().for_each(|c| self.node(c, child)),
            NodeKind::Folder { path, include, exclude } if !missing => {
                self.dir(path, (include, exclude), node.expanded, child)
            }
            _ => {}
        }
    }

    /// Lists `dir` on first expansion; afterwards the cache answers until a refresh drops it.
    fn dir(&mut self, dir: &Path, globs: Globs<'_>, open: bool, place: Place) {
        if open && !self.listings.contains_key(dir) {
            let listing = list_dir(dir, globs.0, globs.1).map_err(|e| format!("{}: {e}", dir.display()));
            if let Err(msg) = &listing {
                self.events.push(TreeEvent::Error(msg.clone()));
            }
            self.listings.insert(dir.to_path_buf(), Arc::new(listing));
        }
        let listing = self.listings.get(dir).cloned();
        match listing.as_deref() {
            Some(Ok(entries)) if open || self.all => entries.iter().for_each(|e| self.entry(e, globs, place)),
            Some(Err(msg)) if open => {
                self.push(Row {
                    key: RowKey::Note(dir.to_path_buf()),
                    parent: place.parent,
                    depth: place.depth,
                    label: msg.clone(),
                    kind: RowKind::Note,
                    path: None,
                    expanded: false,
                    color: None,
                    own_color: None,
                    missing: false,
                    group: place.group,
                });
            }
            _ => {}
        }
    }

    fn entry(&mut self, entry: &DirEntry, globs: Globs<'_>, place: Place) {
        let open = entry.is_dir && self.open_dirs.contains(&entry.path);
        let index = self.push(Row {
            key: RowKey::Entry(entry.path.clone()),
            parent: place.parent,
            depth: place.depth,
            label: entry.name.clone(),
            kind: if entry.is_dir { RowKind::Dir } else { RowKind::EntryFile },
            path: Some(entry.path.clone()),
            expanded: open,
            color: place.color,
            own_color: None,
            missing: false,
            group: place.group,
        });
        if entry.is_dir {
            self.dir(&entry.path, globs, open, place.below(index, place.color, place.group));
        }
    }
}

/// Rows below the (hidden) root group.
pub(crate) fn collect(ws: &Workspace, mut lister: Lister<'_>) -> Vec<Row> {
    let place = Place { depth: 0, parent: None, color: ws.root.color, group: ws.root.id };
    ws.root.children.iter().for_each(|c| lister.node(c, place));
    lister.rows
}

/// Rows whose label fuzzy-matches `pattern`, plus their ancestors; ancestors show as expanded.
pub(crate) fn apply_filter(rows: Vec<Row>, pattern: &str) -> Vec<Row> {
    let matched: Vec<bool> =
        rows.iter().map(|r| r.kind != RowKind::Note && fuzzy_score(pattern, &r.label).is_some()).collect();
    let parents: Vec<Option<usize>> = rows.iter().map(|r| r.parent).collect();
    let visible = visible_with_ancestors(&parents, &matched);
    let mut has_visible_child = vec![false; rows.len()];
    parents
        .iter()
        .zip(&visible)
        .filter_map(|(parent, shown)| parent.filter(|_| *shown))
        .for_each(|p| has_visible_child[p] = true);
    rows.into_iter()
        .zip(visible)
        .zip(has_visible_child)
        .filter(|((_, shown), _)| *shown)
        .map(|((mut row, _), open)| {
            row.expanded = open;
            row
        })
        .collect()
}

/// Matched rows and all their ancestors. Requires pre-order: every parent index is lower than its child's.
pub(crate) fn visible_with_ancestors(parents: &[Option<usize>], matched: &[bool]) -> Vec<bool> {
    let mut visible = matched.to_vec();
    (0..parents.len()).rev().for_each(|i| {
        if let Some(p) = parents[i].filter(|_| visible[i]) {
            visible[p] = true;
        }
    });
    visible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(ws: &Workspace, listings: &mut HashMap<PathBuf, Listing>, events: &mut Vec<TreeEvent>) -> Vec<Row> {
        let (open_dirs, missing) = (HashSet::new(), HashSet::new());
        let lister =
            Lister { listings, open_dirs: &open_dirs, missing: &missing, events, all: false, rows: Vec::new() };
        collect(ws, lister)
    }

    fn labels(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.label.as_str()).collect()
    }

    #[test]
    fn ancestors_of_matches_stay_visible() {
        // 0 group > 1 group > 2 file; 3 file; 4 group > 5 file
        let parents = [None, Some(0), Some(1), None, None, Some(4)];
        let matched = [false, false, true, false, false, false];
        assert_eq!(visible_with_ancestors(&parents, &matched), [true, true, true, false, false, false]);
    }

    #[test]
    fn filter_finds_files_in_collapsed_groups() {
        let mut ws = Workspace::new("t");
        let root = ws.root.id;
        let group = ws.add_group(root, "Docs").expect("add group");
        ws.add_file(group, Path::new("report.txt")).expect("add file");
        ws.add_file(root, Path::new("other.txt")).expect("add file");
        ws.set_expanded(group, false);
        let (mut listings, mut events) = (HashMap::new(), Vec::new());
        assert_eq!(labels(&walk(&ws, &mut listings, &mut events)), ["Docs", "other.txt"]);
        let (open_dirs, missing) = (HashSet::new(), HashSet::new());
        let all = Lister {
            listings: &mut listings,
            open_dirs: &open_dirs,
            missing: &missing,
            events: &mut events,
            all: true,
            rows: Vec::new(),
        };
        let filtered = apply_filter(collect(&ws, all), "rep");
        assert_eq!(labels(&filtered), ["Docs", "report.txt"]);
        assert!(filtered[0].expanded);
    }

    #[test]
    fn linked_folder_lists_once_and_reports_errors_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("a.txt"), "a").expect("write");
        let mut ws = Workspace::new("t");
        let root = ws.root.id;
        let folder = ws.add_folder(root, dir.path(), vec![], vec![]).expect("add folder");
        let gone = ws.add_folder(root, &dir.path().join("gone"), vec![], vec![]).expect("add folder");
        ws.set_expanded(folder, true);
        ws.set_expanded(gone, true);
        let (mut listings, mut events) = (HashMap::new(), Vec::new());
        let rows = walk(&ws, &mut listings, &mut events);
        assert_eq!(
            rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
            [RowKind::Folder, RowKind::Dir, RowKind::EntryFile, RowKind::Folder, RowKind::Note]
        );
        assert_eq!(events.len(), 1);
        std::fs::write(dir.path().join("b.txt"), "b").expect("write");
        let rows = walk(&ws, &mut listings, &mut events);
        assert_eq!(rows.len(), 5, "cached listing is reused until refreshed");
        assert_eq!(events.len(), 1, "a failed listing is reported once");
    }
}
