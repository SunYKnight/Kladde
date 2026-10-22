use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ensure, WsError};
use crate::fsops::{rename_path_on_disk, Filter};
use crate::paths::{absolute, from_stored, normalize, same_path, strip_base, to_stored};
use crate::session::Session;

const ROOT_ID: NodeId = 0;

pub type NodeId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Node colors offered in the UI.
pub const PALETTE: [Rgb; 12] = [
    Rgb(0xE5, 0x39, 0x35),
    Rgb(0xFB, 0x8C, 0x00),
    Rgb(0xFD, 0xD8, 0x35),
    Rgb(0x7C, 0xB3, 0x42),
    Rgb(0x43, 0xA0, 0x47),
    Rgb(0x00, 0x89, 0x7B),
    Rgb(0x00, 0xAC, 0xC1),
    Rgb(0x1E, 0x88, 0xE5),
    Rgb(0x39, 0x49, 0xAB),
    Rgb(0x8E, 0x24, 0xAA),
    Rgb(0xD8, 0x1B, 0x60),
    Rgb(0x75, 0x75, 0x75),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    Group,
    File { path: PathBuf },
    Folder { path: PathBuf, include: Vec<String>, exclude: Vec<String> },
}

/// Tree node; only `Group` nodes have children.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub color: Option<Rgb>,
    #[serde(default)]
    pub expanded: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

impl Node {
    /// Alias, else file/folder name, else "Group".
    pub fn display_name(&self) -> String {
        match (&self.alias, self.path()) {
            (Some(alias), _) => alias.clone(),
            (None, Some(path)) => path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned(),
            (None, None) => "Group".to_string(),
        }
    }

    /// Path of File and Folder nodes.
    pub fn path(&self) -> Option<&Path> {
        match &self.kind {
            NodeKind::Group => None,
            NodeKind::File { path } | NodeKind::Folder { path, .. } => Some(path),
        }
    }

    fn path_mut(&mut self) -> Option<&mut PathBuf> {
        match &mut self.kind {
            NodeKind::Group => None,
            NodeKind::File { path } | NodeKind::Folder { path, .. } => Some(path),
        }
    }

    fn is_group(&self) -> bool {
        matches!(self.kind, NodeKind::Group)
    }
}

fn find(node: &Node, id: NodeId) -> Option<&Node> {
    if node.id == id {
        Some(node)
    } else {
        node.children.iter().find_map(|c| find(c, id))
    }
}

fn find_mut(node: &mut Node, id: NodeId) -> Option<&mut Node> {
    if node.id == id {
        Some(node)
    } else {
        node.children.iter_mut().find_map(|c| find_mut(c, id))
    }
}

fn find_parent(node: &Node, id: NodeId) -> Option<NodeId> {
    if node.children.iter().any(|c| c.id == id) {
        Some(node.id)
    } else {
        node.children.iter().find_map(|c| find_parent(c, id))
    }
}

/// Pre-order walk passing each node with its effective color (own or inherited).
fn visit<'a>(node: &'a Node, inherited: Option<Rgb>, f: &mut dyn FnMut(&'a Node, Option<Rgb>)) {
    let color = node.color.or(inherited);
    f(node, color);
    node.children.iter().for_each(|c| visit(c, color, f));
}

fn visit_mut(node: &mut Node, f: &mut dyn FnMut(&mut Node)) {
    f(node);
    node.children.iter_mut().for_each(|c| visit_mut(c, f));
}

fn validate(node: &Node, ids: &mut HashSet<NodeId>) -> Result<(), WsError> {
    ensure(ids.insert(node.id), &format!("duplicate node id {}", node.id))?;
    ensure(node.is_group() || node.children.is_empty(), "only groups may have children")?;
    if let NodeKind::Folder { include, exclude, .. } = &node.kind {
        Filter::new(include, exclude)?;
    }
    node.children.iter().try_for_each(|c| validate(c, ids))
}

/// One workspace as stored in the global workspace file; field order here is the field order in the file.
#[derive(Serialize, Deserialize)]
pub(crate) struct StoredWorkspace {
    name: String,
    #[serde(default)]
    folded: bool,
    next_id: NodeId,
    root: Node,
    #[serde(default)]
    session: Session,
}

/// Tree of groups, files and linked folders plus the workspace's session.
/// Paths are absolute in memory; the workspace store decides how they are written.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub name: String,
    pub root: Node,
    pub session: Session,
    /// Collapsed to its name in the workspace tab strip.
    pub folded: bool,
    next_id: NodeId,
    dirty: bool,
}

fn map_paths(root: &mut Node, session: &mut Session, f: &dyn Fn(&Path) -> PathBuf) {
    visit_mut(root, &mut |n| {
        if let Some(p) = n.path_mut() {
            *p = f(p);
        }
    });
    session.tabs.iter_mut().filter_map(|t| t.path.as_mut()).for_each(|p| *p = f(p));
}

impl Workspace {
    pub fn new(name: &str) -> Workspace {
        let root =
            Node { id: ROOT_ID, kind: NodeKind::Group, alias: None, color: None, expanded: true, children: vec![] };
        Workspace {
            name: name.to_string(),
            root,
            session: Session::default(),
            folded: false,
            next_id: ROOT_ID + 1,
            dirty: false,
        }
    }

    /// Validates a stored workspace; relative paths resolve against `base`.
    pub(crate) fn from_stored(stored: StoredWorkspace, base: Option<&Path>) -> Result<Workspace, WsError> {
        ensure(stored.root.is_group(), "root node must be a group")?;
        let mut ids = HashSet::new();
        validate(&stored.root, &mut ids)?;
        let (mut root, mut session) = (stored.root, stored.session);
        if let Some(base) = base {
            map_paths(&mut root, &mut session, &|p| from_stored(p, base));
        }
        let max_id = ids.into_iter().max().unwrap_or(ROOT_ID);
        Ok(Workspace {
            name: stored.name,
            root,
            session,
            folded: stored.folded,
            next_id: stored.next_id.max(max_id + 1),
            dirty: false,
        })
    }

    /// Stored form; with `base`, paths are written relative to it when possible, else absolute.
    pub(crate) fn to_stored(&self, base: Option<&Path>) -> StoredWorkspace {
        let (mut root, mut session) = (self.root.clone(), self.session.clone());
        if let Some(base) = base {
            map_paths(&mut root, &mut session, &|p| PathBuf::from(to_stored(p, base)));
        }
        StoredWorkspace { name: self.name.clone(), folded: self.folded, next_id: self.next_id, root, session }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn mark_clean(&mut self) {
        self.dirty = false;
    }

    pub fn set_folded(&mut self, folded: bool) {
        if self.folded != folded {
            self.folded = folded;
            self.dirty = true;
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        find(&self.root, id)
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        find_mut(&mut self.root, id)
    }

    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        find_parent(&self.root, id)
    }

    fn add(
        &mut self,
        parent: NodeId,
        kind: NodeKind,
        alias: Option<String>,
        expanded: bool,
    ) -> Result<NodeId, WsError> {
        let id = self.next_id;
        let target = self.node_mut(parent).filter(|n| n.is_group());
        let target = target.ok_or_else(|| WsError::Invalid(format!("node {parent} is not a group")))?;
        target.children.push(Node { id, kind, alias, color: None, expanded, children: vec![] });
        self.next_id += 1;
        self.dirty = true;
        Ok(id)
    }

    pub fn add_group(&mut self, parent: NodeId, name: &str) -> Result<NodeId, WsError> {
        self.add(parent, NodeKind::Group, Some(name.to_string()), true)
    }

    pub fn add_file(&mut self, parent: NodeId, path: &Path) -> Result<NodeId, WsError> {
        self.add(parent, NodeKind::File { path: absolute(path)? }, None, false)
    }

    /// `Invalid` for malformed globs, so stored folders always have usable filters.
    pub fn add_folder(
        &mut self,
        parent: NodeId,
        path: &Path,
        include: Vec<String>,
        exclude: Vec<String>,
    ) -> Result<NodeId, WsError> {
        Filter::new(&include, &exclude)?;
        self.add(parent, NodeKind::Folder { path: absolute(path)?, include, exclude }, None, false)
    }

    /// Detaches a node with its subtree; the root cannot be removed.
    pub fn remove(&mut self, id: NodeId) -> Option<Node> {
        let parent = self.parent_of(id)?;
        let siblings = &mut self.node_mut(parent)?.children;
        let index = siblings.iter().position(|c| c.id == id)?;
        let node = siblings.remove(index);
        self.dirty = true;
        Some(node)
    }

    /// Moves `id` into group `new_parent` at `index` (counted after detaching, clamped).
    pub fn move_node(&mut self, id: NodeId, new_parent: NodeId, index: usize) -> Result<(), WsError> {
        ensure(id != self.root.id, "the root cannot be moved")?;
        let moving = self.node(id).ok_or_else(|| WsError::Invalid(format!("unknown node {id}")))?;
        ensure(find(moving, new_parent).is_none(), "cannot move a node into itself or its descendants")?;
        ensure(self.node(new_parent).is_some_and(Node::is_group), "target is not a group")?;
        let node = self.remove(id).expect("node exists and is not the root");
        let target = self.node_mut(new_parent).expect("target lies outside the moved subtree");
        let at = index.min(target.children.len());
        target.children.insert(at, node);
        Ok(())
    }

    pub fn set_alias(&mut self, id: NodeId, alias: Option<String>) {
        if let Some(n) = self.node_mut(id) {
            n.alias = alias.filter(|a| !a.is_empty());
            self.dirty = true;
        }
    }

    pub fn set_color(&mut self, id: NodeId, color: Option<Rgb>) {
        if let Some(n) = self.node_mut(id) {
            n.color = color;
            self.dirty = true;
        }
    }

    pub fn set_expanded(&mut self, id: NodeId, expanded: bool) {
        if let Some(n) = self.node_mut(id) {
            n.expanded = expanded;
            self.dirty = true;
        }
    }

    /// Renames the node's file or folder on disk and updates all affected nodes; returns (old, new).
    pub fn rename_on_disk(&mut self, id: NodeId, new_name: &str) -> Result<(PathBuf, PathBuf), WsError> {
        let node = self.node(id).ok_or_else(|| WsError::Invalid(format!("unknown node {id}")))?;
        let old = node.path().ok_or_else(|| WsError::Invalid("groups exist only in the workspace".into()))?;
        let old = old.to_path_buf();
        let new = rename_path_on_disk(&old, new_name)?;
        self.path_renamed(&old, &new);
        Ok((old, new))
    }

    /// Re-targets every File/Folder node at or under `old` to the same place under `new`.
    pub fn path_renamed(&mut self, old: &Path, new: &Path) {
        let new = normalize(new);
        let mut changed = false;
        visit_mut(&mut self.root, &mut |n| {
            if let Some(p) = n.path_mut() {
                if let Some(rest) = strip_base(p, old) {
                    *p = if rest.as_os_str().is_empty() { new.clone() } else { new.join(rest) };
                    changed = true;
                }
            }
        });
        self.dirty |= changed;
    }

    /// First File/Folder node (pre-order) with exactly this path.
    pub fn find_by_path(&self, path: &Path) -> Option<NodeId> {
        let mut found = None;
        visit(&self.root, None, &mut |n, _| {
            if found.is_none() && n.path().is_some_and(|p| same_path(p, path)) {
                found = Some(n.id);
            }
        });
        found
    }

    /// Color of the File node for `path` (own or nearest coloured ancestor); otherwise that of the deepest
    /// linked Folder containing `path`.
    pub fn color_for_path(&self, path: &Path) -> Option<Rgb> {
        let mut file_color = None;
        let mut folder: Option<(usize, Option<Rgb>)> = None;
        visit(&self.root, None, &mut |n, color| match &n.kind {
            NodeKind::File { path: p } if file_color.is_none() && same_path(p, path) => file_color = color,
            NodeKind::Folder { path: p, .. } if strip_base(path, p).is_some() => {
                let depth = p.components().count();
                if folder.is_none_or(|(d, _)| depth > d) {
                    folder = Some((depth, color));
                }
            }
            _ => {}
        });
        file_color.or(folder.and_then(|(_, c)| c))
    }

    /// File/Folder nodes whose path no longer exists.
    pub fn missing(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        visit(&self.root, None, &mut |n, _| {
            if n.path().is_some_and(|p| !p.exists()) {
                out.push(n.id);
            }
        });
        out
    }

    /// Points a File/Folder node at a new (absolute) location.
    pub fn relocate(&mut self, id: NodeId, new_path: &Path) {
        if let Some(p) = self.node_mut(id).and_then(Node::path_mut) {
            *p = normalize(new_path);
            self.dirty = true;
        }
    }

    /// File nodes plus a recursive scan of linked folders (filters applied, symlinked dirs not followed),
    /// de-duplicated, at most `limit`. Unreadable directories are left out.
    pub fn all_files(&self, limit: usize) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        visit(&self.root, None, &mut |n, _| {
            match &n.kind {
                NodeKind::File { path } if out.len() < limit && seen.insert(path.clone()) => out.push(path.clone()),
                NodeKind::Folder { path, include, exclude } => {
                    // Globs were validated when the folder was added or loaded.
                    if let Ok(filter) = Filter::new(include, exclude) {
                        filter.scan(path, limit, &mut seen, &mut out);
                    }
                }
                _ => {}
            }
        });
        out
    }
}
