use kladde_workspace::{Node, NodeId, NodeKind, Workspace};

/// Where a dragged node lands relative to the row under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropPos {
    Before,
    After,
    Into,
}

/// Upper third = before, lower third = after; the middle drops into groups and otherwise splits at half height.
pub(crate) fn drop_position(top: f32, bottom: f32, y: f32, is_group: bool) -> DropPos {
    let third = (bottom - top) / 3.0;
    if y < top + third {
        DropPos::Before
    } else if y >= bottom - third {
        DropPos::After
    } else if is_group {
        DropPos::Into
    } else if y < (top + bottom) / 2.0 {
        DropPos::Before
    } else {
        DropPos::After
    }
}

fn subtree_contains(node: &Node, id: NodeId) -> bool {
    node.id == id || node.children.iter().any(|c| subtree_contains(c, id))
}

/// Group and index for `Workspace::move_node` (counted after detaching `dragged`) when `dragged` is dropped at
/// `pos` relative to `target`; `None` if the target lies in the dragged subtree or cannot hold children.
pub(crate) fn drop_target(ws: &Workspace, dragged: NodeId, target: NodeId, pos: DropPos) -> Option<(NodeId, usize)> {
    let moving = ws.node(dragged)?;
    let parent_id = match pos {
        DropPos::Into => target,
        DropPos::Before | DropPos::After => ws.parent_of(target)?,
    };
    let parent = ws.node(parent_id).filter(|n| matches!(n.kind, NodeKind::Group))?;
    let index = match pos {
        DropPos::Into => Some(parent.children.len()),
        DropPos::Before | DropPos::After => parent.children.iter().position(|c| c.id == target).map(|t| {
            let at = t + usize::from(pos == DropPos::After);
            let detached_before = parent.children.iter().take(at).any(|c| c.id == dragged);
            at - usize::from(detached_before)
        }),
    }?;
    (!subtree_contains(moving, target)).then_some((parent_id, index))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn names(ws: &Workspace, group: NodeId) -> Vec<String> {
        let node = ws.node(group).expect("group exists");
        node.children.iter().map(Node::display_name).collect()
    }

    fn drop(ws: &mut Workspace, dragged: NodeId, target: NodeId, pos: DropPos) {
        let (parent, index) = drop_target(ws, dragged, target, pos).expect("valid drop");
        ws.move_node(dragged, parent, index).expect("move succeeds");
    }

    fn three_files() -> (Workspace, [NodeId; 3]) {
        let mut ws = Workspace::new("t");
        let root = ws.root.id;
        let ids = ["a", "b", "c"].map(|n| ws.add_file(root, Path::new(n)).expect("add file"));
        (ws, ids)
    }

    #[test]
    fn thirds_decide_before_after_and_into() {
        assert_eq!(drop_position(0.0, 30.0, 5.0, true), DropPos::Before);
        assert_eq!(drop_position(0.0, 30.0, 15.0, true), DropPos::Into);
        assert_eq!(drop_position(0.0, 30.0, 25.0, true), DropPos::After);
        assert_eq!(drop_position(0.0, 30.0, 12.0, false), DropPos::Before);
        assert_eq!(drop_position(0.0, 30.0, 18.0, false), DropPos::After);
    }

    #[test]
    fn reorder_within_same_group_accounts_for_detaching() {
        let (mut ws, [a, _, c]) = three_files();
        drop(&mut ws, a, c, DropPos::After);
        assert_eq!(names(&ws, ws.root.id), ["b", "c", "a"]);
        let (mut ws, [a, _, c]) = three_files();
        drop(&mut ws, a, c, DropPos::Before);
        assert_eq!(names(&ws, ws.root.id), ["b", "a", "c"]);
        let (mut ws, [a, _, c]) = three_files();
        drop(&mut ws, c, a, DropPos::Before);
        assert_eq!(names(&ws, ws.root.id), ["c", "a", "b"]);
    }

    #[test]
    fn into_group_appends_and_own_subtree_is_refused() {
        let (mut ws, [a, b, _]) = three_files();
        let root = ws.root.id;
        let group = ws.add_group(root, "g").expect("add group");
        let inner = ws.add_group(group, "inner").expect("add group");
        drop(&mut ws, b, group, DropPos::Into);
        drop(&mut ws, a, group, DropPos::Into);
        assert_eq!(names(&ws, group), ["inner", "b", "a"]);
        assert_eq!(drop_target(&ws, group, inner, DropPos::Into), None);
        assert_eq!(drop_target(&ws, group, group, DropPos::Before), None);
        assert_eq!(drop_target(&ws, group, a, DropPos::After), None);
        assert_eq!(drop_target(&ws, a, b, DropPos::Into), None);
    }
}
