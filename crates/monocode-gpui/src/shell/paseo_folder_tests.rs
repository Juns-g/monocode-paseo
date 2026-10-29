//! Folders on Paseo rows. Folders are native-only UI state; these tests drive
//! every way of creating one in both Paseo grouping modes and both layouts.
use super::super::sidebar_list::Row;
use super::*;
use gpui::{TestAppContext, VisualTestContext, WindowHandle};
use monocode_core::{
    session_groups::Folder,
    store::{Session, Store},
};
use serde_json::json;

/// (status grouping, classic layout)
const MODES: [(bool, bool); 4] = [(false, false), (false, true), (true, false), (true, true)];

struct Env {
    window: WindowHandle<Shell>,
    store: crate::runtime::Database,
    saved: String,
    project: String,
    /// Four loose top-level rows, newest first.
    ids: [i64; 4],
    /// A sub agent nested on `ids[0]`'s rail.
    kid: i64,
}

fn open_window(
    cx: &mut TestAppContext,
    store: &crate::runtime::Database,
    project: &str,
    saved: &str,
) -> WindowHandle<Shell> {
    let store = store.clone();
    let summaries: Vec<Session> = locked(&store).unwrap().sessions().unwrap();
    let (project, saved) = (project.to_string(), saved.to_string());
    cx.add_window(move |_, cx| Shell::new(store, summaries, project, Some(saved), cx))
}

impl Env {
    fn new(cx: &mut TestAppContext, status: bool, classic: bool) -> Self {
        cx.update(|cx| {
            crate::shell::bind_keys(cx);
            crate::input::bind(cx);
            crate::popover::bind(cx);
        });
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let project = store
            .remember_remote_project("paseo-folders", &json!({"id":"p","cwd":"/fixture"}))
            .unwrap();
        let registered = store
            .sync_remote_summaries(
                &project,
                &[
                    json!({"id":"alpha","title":"Alpha","harness":"codex","updatedAt":4000}),
                    json!({"id":"beta","title":"Beta","harness":"codex","updatedAt":3000}),
                    json!({"id":"gamma","title":"Gamma","harness":"codex","updatedAt":2000}),
                    json!({"id":"delta","title":"Delta","harness":"codex","updatedAt":1000}),
                    json!({"id":"kid","title":"Kid","harness":"codex","paseoParentAgentId":"alpha","updatedAt":500}),
                ],
            )
            .unwrap();
        let ids = [0, 1, 2, 3].map(|ix| registered[ix].summary.id);
        let kid = registered[4].summary.id;
        let mut state = Workspace::default();
        state.add_project(&project.key);
        state.open(ids[0]);
        state.classic_layout = classic;
        state
            .preferences
            .extra
            .insert(crate::paseo_host::PROJECT_SELECTION.into(), json!({}));
        state.preferences.extra.insert(
            "paseo_group".into(),
            json!(if status { "status" } else { "project" }),
        );
        crate::paseo_host::restrict_workspace(&mut state);
        let saved = serde_json::to_string(&state).unwrap();
        let store = Arc::new(Mutex::new(store));
        let window = open_window(cx, &store, &project.key, &saved);
        Self {
            window,
            store,
            saved,
            project: project.key,
            ids,
            kid,
        }
    }
    /// A fresh Shell on the same store, as after an app restart.
    fn reload(&self, cx: &mut TestAppContext) -> WindowHandle<Shell> {
        open_window(cx, &self.store, &self.project, &self.saved)
    }
    fn stored(&self) -> Groups {
        locked(&self.store).unwrap().groups(&self.project).unwrap()
    }
}

fn rows_of(window: &WindowHandle<Shell>, cx: &mut TestAppContext) -> Vec<Row> {
    window
        .update(cx, |shell, _, cx| {
            shell.invalidate_chrome(cx);
            shell.refresh_sidebar_structure(cx);
            shell.sidebar_list.rows.clone()
        })
        .unwrap()
}
fn member(row: &Row) -> Option<i64> {
    match row {
        Row::Session(id) | Row::GroupedSession { id, .. } | Row::ClassicGroupMember { id, .. } => {
            Some(*id)
        }
        _ => None,
    }
}
fn occurrences(rows: &[Row], id: i64) -> usize {
    rows.iter().filter(|row| member(row) == Some(id)).count()
}
fn header(rows: &[Row], folder: &str) -> Option<usize> {
    rows.iter()
        .position(|row| matches!(row, Row::Group(Entry::Folder { id, .. }) if id == folder))
}
/// The members listed under the folder's header, if it has one.
fn folder_members(rows: &[Row], folder: &str) -> Option<Vec<i64>> {
    let at = header(rows, folder)?;
    Some(
        rows[at + 1..]
            .iter()
            .map_while(|row| match row {
                Row::GroupedSession { id, .. } | Row::ClassicGroupMember { id, .. } => Some(*id),
                _ => None,
            })
            .collect(),
    )
}
fn action_ids(items: Vec<MenuItem>) -> Vec<String> {
    items
        .into_iter()
        .filter_map(|item| match item {
            MenuItem::Action(action) => Some(action.id.to_string()),
            _ => None,
        })
        .collect()
}
fn selector(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}
fn label((status, classic): (bool, bool)) -> String {
    format!("status={status} classic={classic}")
}

/// New folder from the session menu, its rename row, the saved write and a
/// restart.
fn menu_creation(cx: &mut TestAppContext, mode: (bool, bool)) {
    let (status, classic) = mode;
    let env = Env::new(cx, status, classic);
    let alpha = env.ids[0];
    let at = label(mode);
    let folder = env
        .window
        .update(cx, |shell, w, cx| {
            assert_eq!(shell.workspace.classic_layout, classic, "{at}");
            shell.refresh_sidebar_structure(cx);
            assert!(
                action_ids(shell.session_menu_items(alpha, cx)).contains(&"folder-new".into()),
                "{at}"
            );
            shell.run_session_menu(alpha, "folder-new", w, cx);
            let folder = shell.project_groups().folders[0].id.clone();
            assert!(
                matches!(&shell.renaming.as_ref().expect(&at).target,
                    RenameTarget::Folder(id) if *id == folder),
                "{at}: the rename row opens right after creation"
            );
            assert!(
                shell
                    .rename_row(&RenameTarget::Folder(folder.clone()), cx)
                    .is_some(),
                "{at}"
            );
            folder
        })
        .unwrap();
    let rows = rows_of(&env.window, cx);
    assert_eq!(
        folder_members(&rows, &folder),
        Some(vec![alpha]),
        "{at}: the folder header and its member are listed: {rows:?}"
    );
    assert_eq!(occurrences(&rows, alpha), 1, "{at}: no duplicate row");
    env.window
        .update(cx, |shell, w, cx| {
            shell
                .renaming
                .as_ref()
                .unwrap()
                .input
                .update(cx, |input, cx| input.set_text("Work".into(), cx));
            shell.finish_rename(true, w, cx);
            assert!(shell.renaming.is_none(), "{at}");
        })
        .unwrap();
    cx.run_until_parked();
    let stored = env.stored();
    assert_eq!(stored.folders.len(), 1, "{at}: the write is persisted");
    assert_eq!(stored.folders[0].name, "Work", "{at}");
    assert_eq!(stored.folders[0].session_ids, [alpha], "{at}");
    env.window
        .update(cx, |shell, _, _| {
            assert!(shell.error.is_none(), "{at}: {:?}", shell.error)
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(env.window.into(), cx);
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds(selector(format!("session-group-folder:{folder}")))
            .is_some(),
        "{at}: the folder header is on screen"
    );
    let reloaded = env.reload(cx);
    let rows = rows_of(&reloaded, cx);
    assert_eq!(
        folder_members(&rows, &folder),
        Some(vec![alpha]),
        "{at}: survives a restart"
    );
    assert_eq!(occurrences(&rows, alpha), 1, "{at}");
    reloaded
        .update(cx, |shell, _, _| {
            assert_eq!(shell.project_groups().folders[0].name, "Work", "{at}")
        })
        .unwrap();
}

#[gpui::test]
fn menu_creates_a_folder_project_grouping_new_layout(cx: &mut TestAppContext) {
    menu_creation(cx, MODES[0]);
}
#[gpui::test]
fn menu_creates_a_folder_project_grouping_classic_layout(cx: &mut TestAppContext) {
    menu_creation(cx, MODES[1]);
}
#[gpui::test]
fn menu_creates_a_folder_status_grouping_new_layout(cx: &mut TestAppContext) {
    menu_creation(cx, MODES[2]);
}
#[gpui::test]
fn menu_creates_a_folder_status_grouping_classic_layout(cx: &mut TestAppContext) {
    menu_creation(cx, MODES[3]);
}

fn multi_select_add_remove(cx: &mut TestAppContext, mode: (bool, bool)) {
    let env = Env::new(cx, mode.0, mode.1);
    let [alpha, beta, gamma, _] = env.ids;
    let at = label(mode);
    let folder = env
        .window
        .update(cx, |shell, w, cx| {
            shell.refresh_sidebar_structure(cx);
            shell.selected_sessions.extend([alpha, beta]);
            shell.run_session_menu(alpha, "folder-new", w, cx);
            shell.finish_rename(false, w, cx);
            shell.selected_sessions.clear();
            let folders = shell.project_groups().folders;
            assert_eq!(folders.len(), 1, "{at}");
            assert_eq!(folders[0].session_ids, [alpha, beta], "{at}");
            folders[0].id.clone()
        })
        .unwrap();
    let rows = rows_of(&env.window, cx);
    assert_eq!(
        folder_members(&rows, &folder),
        Some(vec![alpha, beta]),
        "{at}: {rows:?}"
    );
    // Add a third row from its menu, then take it out again.
    env.window
        .update(cx, |shell, w, cx| {
            let items = action_ids(shell.session_menu_items(gamma, cx));
            assert!(items.contains(&format!("folder-add:{folder}")), "{at}");
            assert!(!items.contains(&"folder-remove".into()), "{at}");
            shell.run_session_menu(gamma, &format!("folder-add:{folder}"), w, cx);
        })
        .unwrap();
    let rows = rows_of(&env.window, cx);
    assert_eq!(
        folder_members(&rows, &folder),
        Some(vec![alpha, beta, gamma]),
        "{at}"
    );
    cx.run_until_parked();
    assert_eq!(
        env.stored().folders[0].session_ids,
        [alpha, beta, gamma],
        "{at}"
    );
    env.window
        .update(cx, |shell, w, cx| {
            assert!(
                action_ids(shell.session_menu_items(gamma, cx)).contains(&"folder-remove".into())
            );
            shell.run_session_menu(gamma, "folder-remove", w, cx);
        })
        .unwrap();
    let rows = rows_of(&env.window, cx);
    assert_eq!(
        folder_members(&rows, &folder),
        Some(vec![alpha, beta]),
        "{at}"
    );
    assert_eq!(occurrences(&rows, gamma), 1, "{at}: back in the list");
    cx.run_until_parked();
    assert_eq!(env.stored().folders[0].session_ids, [alpha, beta], "{at}");
}

#[gpui::test]
fn multi_select_add_and_remove_project_new(cx: &mut TestAppContext) {
    multi_select_add_remove(cx, MODES[0]);
}
#[gpui::test]
fn multi_select_add_and_remove_project_classic(cx: &mut TestAppContext) {
    multi_select_add_remove(cx, MODES[1]);
}
#[gpui::test]
fn multi_select_add_and_remove_status_new(cx: &mut TestAppContext) {
    multi_select_add_remove(cx, MODES[2]);
}
#[gpui::test]
fn multi_select_add_and_remove_status_classic(cx: &mut TestAppContext) {
    multi_select_add_remove(cx, MODES[3]);
}

/// Dropping a row on a row creates a folder; dropping on a header joins it.
fn drag_creation(cx: &mut TestAppContext, mode: (bool, bool)) {
    let env = Env::new(cx, mode.0, mode.1);
    let [alpha, beta, gamma, _] = env.ids;
    let at = label(mode);
    let visual = &mut VisualTestContext::from_window(env.window.into(), cx);
    visual.run_until_parked();
    let bounds = |visual: &mut VisualTestContext, id: i64| {
        visual
            .debug_bounds(selector(format!("sidebar-session-{id}")))
            .unwrap_or_else(|| panic!("{at}: row {id} is on screen"))
    };
    let (a_row, b_row) = (bounds(visual, alpha), bounds(visual, beta));
    env.window
        .update(visual, |shell, _, cx| {
            assert!(
                matches!(shell.sidebar_drop_at(b_row.center(), alpha, cx),
                    Some(ListDrop::Session(id)) if id == beta),
                "{at}: hovering a row offers a new folder"
            );
        })
        .unwrap();
    visual.simulate_mouse_down(a_row.center(), MouseButton::Left, Default::default());
    visual.simulate_mouse_move(b_row.center(), Some(MouseButton::Left), Default::default());
    visual.simulate_mouse_up(b_row.center(), MouseButton::Left, Default::default());
    visual.run_until_parked();
    let folder = env
        .window
        .update(visual, |shell, w, cx| {
            let groups = shell.project_groups();
            assert_eq!(groups.folders.len(), 1, "{at}");
            assert_eq!(groups.folders[0].session_ids, [alpha, beta], "{at}");
            assert!(shell.renaming.is_some(), "{at}: rename row after the drop");
            shell.finish_rename(false, w, cx);
            groups.folders[0].id.clone()
        })
        .unwrap();
    visual.refresh().unwrap();
    visual.run_until_parked();
    assert_eq!(env.stored().folders.len(), 1, "{at}");
    let rows = rows_of(&env.window, visual);
    assert_eq!(
        folder_members(&rows, &folder),
        Some(vec![alpha, beta]),
        "{at}: {rows:?}"
    );
    visual.refresh().unwrap();
    let g_row = bounds(visual, gamma);
    let head = visual
        .debug_bounds(selector(format!("session-group-folder:{folder}")))
        .unwrap_or_else(|| panic!("{at}: header on screen"));
    visual.simulate_mouse_down(g_row.center(), MouseButton::Left, Default::default());
    visual.simulate_mouse_move(head.center(), Some(MouseButton::Left), Default::default());
    visual.simulate_mouse_up(head.center(), MouseButton::Left, Default::default());
    visual.run_until_parked();
    assert_eq!(
        env.stored().folders[0].session_ids,
        [alpha, beta, gamma],
        "{at}"
    );
}

#[gpui::test]
fn drag_creates_a_folder_project_new(cx: &mut TestAppContext) {
    drag_creation(cx, MODES[0]);
}
#[gpui::test]
fn drag_creates_a_folder_project_classic(cx: &mut TestAppContext) {
    drag_creation(cx, MODES[1]);
}
#[gpui::test]
fn drag_creates_a_folder_status_new(cx: &mut TestAppContext) {
    drag_creation(cx, MODES[2]);
}
#[gpui::test]
fn drag_creates_a_folder_status_classic(cx: &mut TestAppContext) {
    drag_creation(cx, MODES[3]);
}

fn inject(shell: &mut Shell, folders: Vec<Folder>) {
    shell.groups.insert(
        shell.workspace.project.clone(),
        Groups {
            folders,
            ..Default::default()
        },
    );
}
fn folder(id: &str, sessions: &[i64]) -> Folder {
    Folder {
        id: id.into(),
        name: id.into(),
        session_ids: sessions.to_vec(),
        ..Default::default()
    }
}

/// A nested sub agent travels with its parent: it offers no folder actions,
/// cannot start a folder by drag, and never leaves an empty folder header.
fn child_rows(cx: &mut TestAppContext, mode: (bool, bool)) {
    let env = Env::new(cx, mode.0, mode.1);
    let [alpha, beta, ..] = env.ids;
    let kid = env.kid;
    let at = label(mode);
    env.window
        .update(cx, |shell, w, cx| {
            inject(shell, vec![folder("work", &[beta])]);
            shell.refresh_sidebar_structure(cx);
            assert!(shell.rails.children.contains(&kid), "{at}");
            let items = action_ids(shell.session_menu_items(kid, cx));
            assert!(
                !items.iter().any(|id| id.starts_with("folder-")),
                "{at}: a child offers no folder actions: {items:?}"
            );
            let items = action_ids(shell.session_menu_items(alpha, cx));
            assert!(items.contains(&"folder-new".into()), "{at}");
            assert!(items.contains(&"folder-add:work".into()), "{at}");
            shell.run_session_menu(kid, "folder-new", w, cx);
            shell.run_session_menu(kid, "folder-add:work", w, cx);
            shell.apply_sidebar_drop(kid, ListDrop::Session(alpha), w, cx);
            shell.apply_sidebar_drop(alpha, ListDrop::Session(kid), w, cx);
            shell.apply_sidebar_drop(kid, ListDrop::Folder("work".into()), w, cx);
            shell.apply_sidebar_drop(kid, ListDrop::Session(beta), w, cx);
            let groups = shell.project_groups();
            assert_eq!(groups.folders.len(), 1, "{at}: {:?}", groups.folders);
            assert_eq!(groups.folders[0].session_ids, [beta], "{at}");
            assert!(shell.renaming.is_none(), "{at}");
            // Only nested children: no header at all. With the parent: the
            // header counts what is visible.
            inject(
                shell,
                vec![folder("kids", &[kid]), folder("both", &[alpha, kid])],
            );
        })
        .unwrap();
    let rows = rows_of(&env.window, cx);
    assert!(
        header(&rows, "kids").is_none(),
        "{at}: no empty header: {rows:?}"
    );
    assert_eq!(folder_members(&rows, "both"), Some(vec![alpha]), "{at}");
    assert_eq!(
        occurrences(&rows, kid),
        0,
        "{at}: the child stays on its rail"
    );
    let at_header = header(&rows, "both").unwrap();
    assert!(
        matches!(&rows[at_header], Row::Group(Entry::Folder { sessions, .. }) if *sessions == [alpha]),
        "{at}: the count is of visible rows"
    );
}

#[gpui::test]
fn child_rows_project_new(cx: &mut TestAppContext) {
    child_rows(cx, MODES[0]);
}
#[gpui::test]
fn child_rows_project_classic(cx: &mut TestAppContext) {
    child_rows(cx, MODES[1]);
}
#[gpui::test]
fn child_rows_status_new(cx: &mut TestAppContext) {
    child_rows(cx, MODES[2]);
}
#[gpui::test]
fn child_rows_status_classic(cx: &mut TestAppContext) {
    child_rows(cx, MODES[3]);
}

#[gpui::test]
fn dragging_a_child_offers_no_drop(cx: &mut TestAppContext) {
    let env = Env::new(cx, false, false);
    let [_, beta, ..] = env.ids;
    let kid = env.kid;
    let visual = &mut VisualTestContext::from_window(env.window.into(), cx);
    visual.run_until_parked();
    let b_row = visual
        .debug_bounds(selector(format!("sidebar-session-{beta}")))
        .unwrap();
    env.window
        .update(visual, |shell, _, cx| {
            assert!(shell.sidebar_drop_at(b_row.center(), kid, cx).is_none());
        })
        .unwrap();
}

/// Status grouping lists folders after Pinned and before the buckets.
fn status_folders(cx: &mut TestAppContext, classic: bool) {
    let env = Env::new(cx, true, classic);
    let [alpha, beta, gamma, delta] = env.ids;
    env.window
        .update(cx, |shell, _, cx| {
            shell.session_summaries.get_mut(&gamma).unwrap().pinned = true;
            inject(shell, vec![folder("work", &[alpha, beta])]);
            let _ = cx;
        })
        .unwrap();
    let rows = rows_of(&env.window, cx);
    let pinned = rows
        .iter()
        .position(|r| matches!(r, Row::Pinned | Row::Group(Entry::Pinned { .. })))
        .expect("pinned section");
    let head = header(&rows, "work").unwrap_or_else(|| panic!("folder header: {rows:?}"));
    let bucket = rows
        .iter()
        .position(|r| matches!(r, Row::Status(..)))
        .unwrap();
    assert!(pinned < head && head < bucket, "{rows:?}");
    assert_eq!(
        folder_members(&rows, "work"),
        Some(vec![alpha, beta]),
        "{rows:?}"
    );
    for id in [alpha, beta, gamma, delta] {
        assert_eq!(occurrences(&rows, id), 1, "{id} is listed once: {rows:?}");
    }
    assert!(
        rows.contains(&Row::Status(crate::paseo_host::StatusBucket::Done, 1)),
        "{rows:?}"
    );
    assert!(
        !rows[..bucket].iter().any(|r| member(r) == Some(delta)),
        "unfoldered rows stay in their bucket"
    );
    // Collapsed folders hide their members, and a search opens them.
    env.window
        .update(cx, |shell, _, cx| {
            shell.update_groups(cx, |g| g.toggle_folder("work"))
        })
        .unwrap();
    let rows = rows_of(&env.window, cx);
    assert_eq!(folder_members(&rows, "work"), Some(vec![]), "{rows:?}");
    assert_eq!(occurrences(&rows, alpha), 0);
    env.window
        .update(cx, |shell, _, _| shell.query = "alpha".into())
        .unwrap();
    let rows = rows_of(&env.window, cx);
    assert_eq!(folder_members(&rows, "work"), Some(vec![alpha]), "{rows:?}");
}
#[gpui::test]
fn status_grouping_lists_folders_new(cx: &mut TestAppContext) {
    status_folders(cx, false);
}
#[gpui::test]
fn status_grouping_lists_folders_classic(cx: &mut TestAppContext) {
    status_folders(cx, true);
}

#[gpui::test]
fn folders_of_a_project_not_loaded_at_startup_are_read_before_a_first_edit(
    cx: &mut TestAppContext,
) {
    let env = Env::new(cx, false, false);
    let second = {
        let store = locked(&env.store).unwrap();
        let project = store
            .remember_remote_project("paseo-folders", &json!({"id":"q","cwd":"/second"}))
            .unwrap();
        let rows = store
            .sync_remote_summaries(
                &project,
                &[
                    json!({"id":"one","title":"One","harness":"codex","updatedAt":900}),
                    json!({"id":"two","title":"Two","harness":"codex","updatedAt":800}),
                ],
            )
            .unwrap();
        let ids = [rows[0].summary.id, rows[1].summary.id];
        store
            .save_groups(
                &project.key,
                &Groups {
                    folders: vec![folder("stored", &[ids[0]])],
                    ..Default::default()
                },
            )
            .unwrap();
        (project.key, ids)
    };
    // The shell starts only knowing the first project.
    let window = env.reload(cx);
    window
        .update(cx, |shell, w, cx| {
            assert!(!shell.groups.contains_key(&second.0));
            shell.workspace.project = second.0.clone();
            shell.invalidate_chrome(cx);
            shell.refresh_sidebar_structure(cx);
            assert_eq!(
                shell.project_groups().folders.len(),
                1,
                "stored folders are read when the project is first shown"
            );
            let rows = shell.sidebar_list.rows.clone();
            assert_eq!(folder_members(&rows, "stored"), Some(vec![second.1[0]]));
            shell.run_session_menu(second.1[1], "folder-new", w, cx);
        })
        .unwrap();
    cx.run_until_parked();
    let saved = locked(&env.store).unwrap().groups(&second.0).unwrap();
    assert_eq!(
        saved.folders.len(),
        2,
        "a first edit keeps the stored folder"
    );
    assert!(saved.folder("stored").is_some());
    // Editing before anything was displayed reads them too.
    let window = env.reload(cx);
    window
        .update(cx, |shell, _, cx| {
            shell.workspace.project = second.0.clone();
            shell.update_groups(cx, |g| g.toggle_folder("stored"));
        })
        .unwrap();
    cx.run_until_parked();
    let saved = locked(&env.store).unwrap().groups(&second.0).unwrap();
    assert_eq!(saved.folders.len(), 2);
    assert!(saved.folder("stored").unwrap().collapsed);
}

#[gpui::test]
fn pin_is_disabled_for_non_anchor_rows_and_ignored_when_run(cx: &mut TestAppContext) {
    let env = Env::new(cx, false, false);
    let [alpha, beta, ..] = env.ids;
    env.window
        .update(cx, |shell, w, cx| {
            let pin = |shell: &Shell, id, cx: &App| {
                shell
                    .session_menu_items(id, cx)
                    .into_iter()
                    .find_map(|item| match item {
                        MenuItem::Action(action) if action.id.as_ref() == "pin" => Some(action),
                        _ => None,
                    })
                    .expect("Pin stays in the menu")
            };
            assert!(
                !pin(shell, alpha, cx).disabled,
                "unknown anchor is pinnable"
            );
            shell
                .remote_details
                .insert(beta, json!({"workspaceAnchor": false}));
            shell
                .remote_details
                .insert(alpha, json!({"workspaceAnchor": true}));
            assert!(pin(shell, beta, cx).disabled);
            assert!(!pin(shell, alpha, cx).disabled);
            shell.run_session_menu(beta, "pin", w, cx);
            assert!(
                !shell.session_summaries[&beta].pinned,
                "ignored defensively"
            );
        })
        .unwrap();
}
