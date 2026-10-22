use std::fs;
use std::path::Path;

use kladde_workspace::*;

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"x").unwrap();
}

/// Store with one workspace `ws` that is active.
fn store_of(ws: Workspace) -> WorkspaceStore {
    WorkspaceStore { workspaces: vec![ws], active: 0 }
}

#[test]
fn portable_store_round_trip_writes_relative_slash_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("kladde-data");
    fs::create_dir_all(&data).unwrap();
    let inner = tmp.path().join("proj").join("src").join("a.txt");
    let outer = tmp.path().join("other").join("b.txt");
    touch(&inner);
    touch(&outer);
    let mut ws = Workspace::new("demo");
    let group = ws.add_group(0, "Sources").unwrap();
    ws.add_file(group, &inner).unwrap();
    let outer_id = ws.add_file(0, &outer).unwrap();
    ws.add_folder(0, &tmp.path().join("proj").join("docs"), vec!["*.md".into()], vec!["build".into()]).unwrap();
    ws.set_color(group, Some(PALETTE[3]));
    ws.set_expanded(group, false);
    ws.set_folded(true);
    ws.session.tabs.push(TabState { path: Some(inner.clone()), title: "a.txt".into(), ..Default::default() });
    let mut store = store_of(ws.clone());
    let file = store_file(&data);
    store.save(&file, Some(&data)).unwrap();
    assert!(!store.is_dirty());

    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"../proj/src/a.txt\""), "{text}");
    assert!(text.contains("\"../other/b.txt\""), "{text}");
    assert!(!text.contains('\\'), "{text}");

    let loaded = WorkspaceStore::load(&file, Some(&data)).unwrap();
    let w = loaded.current();
    assert_eq!(w.name, "demo");
    assert!(w.folded);
    assert_eq!(w.root, ws.root);
    assert_eq!(w.session, ws.session);
    assert_eq!(w.node(outer_id).unwrap().path(), Some(outer.as_path()));
    assert!(w.missing().iter().all(|id| w.node(*id).unwrap().display_name() == "docs"));
}

#[test]
fn store_without_base_keeps_absolute_paths_and_active_index() {
    let tmp = tempfile::tempdir().unwrap();
    let file = store_file(tmp.path());
    let target = tmp.path().join("x").join("a.txt");
    let mut store = WorkspaceStore::default();
    let second = store.add("Notes");
    store.workspaces[second].add_file(0, &target).unwrap();
    store.active = second;
    store.save(&file, None).unwrap();

    let loaded = WorkspaceStore::load(&file, None).unwrap();
    assert_eq!(loaded.workspaces.len(), 2);
    assert_eq!(loaded.active, second);
    assert_eq!(loaded.current().name, "Notes");
    assert_eq!(loaded.current().all_files(10), vec![target]);
}

#[test]
fn missing_store_file_yields_one_default_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::load(&store_file(tmp.path()), None).unwrap();
    assert_eq!(store.workspaces.len(), 1);
    assert_eq!(store.current().name, DEFAULT_WORKSPACE);
}

#[test]
fn ids_stay_unique_after_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let file = store_file(tmp.path());
    let mut ws = Workspace::new("w");
    let a = ws.add_group(0, "a").unwrap();
    let b = ws.add_group(0, "b").unwrap();
    ws.remove(b).unwrap();
    store_of(ws).save(&file, None).unwrap();
    let mut loaded = WorkspaceStore::load(&file, None).unwrap();
    let c = loaded.current_mut().add_group(0, "c").unwrap();
    assert!(c != a && c != b);
    assert!(loaded.is_dirty());
}

#[test]
fn load_rejects_duplicate_ids_and_empty_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let file = store_file(tmp.path());
    let json = r#"{"version":1,"active":0,"workspaces":[{"name":"x","next_id":3,"root":{"id":0,"kind":"Group",
        "children":[{"id":1,"kind":"Group"},{"id":1,"kind":"Group"}]}}]}"#;
    fs::write(&file, json).unwrap();
    assert!(matches!(WorkspaceStore::load(&file, None), Err(WsError::Invalid(_))));
    fs::write(&file, r#"{"version":1,"active":0,"workspaces":[]}"#).unwrap();
    assert!(matches!(WorkspaceStore::load(&file, None), Err(WsError::Invalid(_))));
}

#[test]
fn add_rename_remove_keep_names_unique_and_active_valid() {
    let mut store = WorkspaceStore::default();
    let a = store.add("Notes");
    let b = store.add("notes");
    assert_eq!(store.workspaces[b].name, "notes 2");
    assert!(matches!(store.rename(b, "NOTES"), Err(WsError::Invalid(_))));
    assert!(matches!(store.rename(b, "  "), Err(WsError::Invalid(_))));
    store.rename(b, "Logs").unwrap();
    store.active = b;
    store.remove(a).unwrap();
    assert_eq!(store.current().name, "Logs");
    store.remove(store.active).unwrap();
    assert_eq!(store.current().name, DEFAULT_WORKSPACE);
    assert!(matches!(store.remove(0), Err(WsError::Invalid(_))));
}

#[test]
fn move_node_keeps_tree_valid() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ws = Workspace::new("m");
    let g1 = ws.add_group(0, "g1").unwrap();
    let g2 = ws.add_group(g1, "g2").unwrap();
    let f = ws.add_file(0, &tmp.path().join("f.txt")).unwrap();
    let before = ws.root.clone();

    assert!(ws.move_node(g1, g1, 0).is_err());
    assert!(ws.move_node(g1, g2, 0).is_err());
    assert!(ws.move_node(g2, f, 0).is_err());
    assert!(ws.move_node(0, g1, 0).is_err());
    assert_eq!(ws.root, before);

    ws.move_node(f, g2, 99).unwrap();
    assert_eq!(ws.parent_of(f), Some(g2));
    let g3 = ws.add_group(g2, "g3").unwrap();
    ws.move_node(g3, g2, 0).unwrap();
    let order: Vec<NodeId> = ws.node(g2).unwrap().children.iter().map(|c| c.id).collect();
    assert_eq!(order, vec![g3, f]);
    assert!(ws.remove(0).is_none());
}

#[test]
fn rename_on_disk_updates_nodes_and_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("old");
    let file = dir.join("sub").join("a.txt");
    touch(&file);
    let mut ws = Workspace::new("r");
    let folder = ws.add_folder(0, &dir, vec![], vec![]).unwrap();
    let file_node = ws.add_file(0, &file).unwrap();
    let group = ws.add_group(0, "g").unwrap();

    assert!(ws.rename_on_disk(folder, "a/b").is_err());
    assert!(ws.rename_on_disk(folder, "").is_err());
    assert!(ws.rename_on_disk(group, "x").is_err());

    let (old, new) = ws.rename_on_disk(folder, "new").unwrap();
    assert_eq!(old, dir);
    assert_eq!(new, tmp.path().join("new"));
    assert!(!dir.exists());
    let moved = tmp.path().join("new").join("sub").join("a.txt");
    assert!(moved.exists());
    assert_eq!(ws.node(folder).unwrap().path(), Some(new.as_path()));
    assert_eq!(ws.node(file_node).unwrap().path(), Some(moved.as_path()));

    let (_, renamed) = ws.rename_on_disk(file_node, "b.txt").unwrap();
    assert!(renamed.exists() && !moved.exists());
    assert_eq!(ws.find_by_path(&renamed), Some(file_node));
}

#[test]
fn rename_refuses_to_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    touch(&tmp.path().join("a.txt"));
    touch(&tmp.path().join("b.txt"));
    assert!(rename_path_on_disk(&tmp.path().join("a.txt"), "b.txt").is_err());
    assert!(tmp.path().join("a.txt").exists());
}

#[test]
fn color_inherits_from_groups_and_linked_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let red = PALETTE[0];
    let blue = PALETTE[7];
    let green = PALETTE[4];
    let mut ws = Workspace::new("c");
    let group = ws.add_group(0, "g").unwrap();
    ws.set_color(group, Some(red));
    ws.add_file(group, &t.join("plain.txt")).unwrap();
    let own = ws.add_file(group, &t.join("own.txt")).unwrap();
    ws.set_color(own, Some(blue));
    ws.add_folder(group, &t.join("lib"), vec![], vec![]).unwrap();
    let nested = ws.add_folder(0, &t.join("lib").join("deep"), vec![], vec![]).unwrap();
    ws.set_color(nested, Some(green));
    ws.add_file(0, &t.join("loose.txt")).unwrap();

    assert_eq!(ws.color_for_path(&t.join("plain.txt")), Some(red));
    assert_eq!(ws.color_for_path(&t.join("own.txt")), Some(blue));
    assert_eq!(ws.color_for_path(&t.join("lib").join("x.rs")), Some(red));
    assert_eq!(ws.color_for_path(&t.join("lib").join("deep").join("y.rs")), Some(green));
    assert_eq!(ws.color_for_path(&t.join("loose.txt")), None);
    assert_eq!(ws.color_for_path(&t.join("libx").join("z.rs")), None);
}

#[test]
fn list_dir_filters_and_orders() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    ["B.txt", "a.rs", "x.log", "c.md"].iter().for_each(|n| touch(&t.join(n)));
    ["Zeta", "alpha", "target"].iter().for_each(|d| fs::create_dir(t.join(d)).unwrap());
    let entries = list_dir(t, &["*.rs".into(), "*.txt".into()], &["target".into(), "*.log".into()]).unwrap();
    let names: Vec<(&str, bool)> = entries.iter().map(|e| (e.name.as_str(), e.is_dir)).collect();
    assert_eq!(names, vec![("alpha", true), ("Zeta", true), ("a.rs", false), ("B.txt", false)]);
    assert!(list_dir(t, &["[".into()], &[]).is_err());
}

#[test]
fn all_files_scans_folders_with_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    touch(&t.join("src").join("a.rs"));
    touch(&t.join("src").join("inner").join("b.rs"));
    touch(&t.join("src").join("target").join("c.rs"));
    touch(&t.join("src").join("notes.txt"));
    let mut ws = Workspace::new("a");
    ws.add_file(0, &t.join("src").join("a.rs")).unwrap();
    ws.add_folder(0, &t.join("src"), vec!["*.rs".into()], vec!["target".into()]).unwrap();
    let mut all = ws.all_files(100);
    all.sort();
    assert_eq!(all, vec![t.join("src").join("a.rs"), t.join("src").join("inner").join("b.rs")]);
    assert_eq!(ws.all_files(1).len(), 1);
}

#[test]
fn fuzzy_rank_prefers_word_starts_runs_and_short_names() {
    let candidates = ["src/domain/manifest.txt", "docs/maintenance.md", "lib/m_a_i_n.c", "README.md", "src/main.rs"];
    let ranked: Vec<usize> = fuzzy_rank("main", &candidates, 10).into_iter().map(|(i, _)| i).collect();
    assert_eq!(ranked, vec![4, 1, 2, 0]);
    assert_eq!(fuzzy_rank("main", &candidates, 2).len(), 2);
    assert!(fuzzy_score("MAIN", "src/main.rs").is_some());
    assert!(fuzzy_score("xyz", "src/main.rs").is_none());
}

#[test]
fn settings_tolerate_missing_and_unknown_fields() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(Settings::load(tmp.path()).unwrap(), Settings::default());
    fs::write(tmp.path().join("settings.json"), r#"{"font_size": 20.0, "future_option": [1, 2]}"#).unwrap();
    let s = Settings::load(tmp.path()).unwrap();
    assert_eq!(s.font_size, 20.0);
    assert_eq!(s.tab_width, Settings::default().tab_width);
    assert_eq!(s.theme, ThemeChoice::System);

    let mut s = s;
    s.keybindings.insert("save".into(), "Ctrl+S".into());
    let dir = tmp.path().join("nested");
    s.save(&dir).unwrap();
    assert_eq!(Settings::load(&dir).unwrap(), s);
}

#[test]
fn recent_lists_are_unique_newest_first_and_capped() {
    let mut s = Settings::default();
    (0..25).for_each(|i| s.push_recent_file(Path::new(&format!("/f{i}"))));
    s.push_recent_file(Path::new("/f10"));
    assert_eq!(s.recent_files.len(), 20);
    assert_eq!(s.recent_files[0], Path::new("/f10"));
    assert_eq!(s.recent_files[1], Path::new("/f24"));
    assert_eq!(s.recent_files.iter().filter(|p| *p == Path::new("/f10")).count(), 1);
}
