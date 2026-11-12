//! The side panel, laid out as GitKraken's: a filter over every section, then
//! LOCAL, REMOTE (by remote), WORKSPACES (jj's take on worktrees), PULL
//! REQUESTS and TAGS, each folding away under a header with its count.
//!
//! The panel lists every bookmark and tag of the repository, not only those
//! of the revisions the log has loaded: they come from a query of their own.
//! Clicking one the log does not show widens the log's revset to take it in.

use std::collections::HashSet;

use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::{ClipboardItem, WeakEntity};

use super::*;

/// Revisions with something for the side panel.
const PANEL_REVSET: &str = "bookmarks() | remote_bookmarks() | tags() | working_copies()";
const PANEL_LIMIT: usize = 5000;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum PanelSection {
    Local,
    Remote,
    Workspaces,
    Tags,
}

/// The sections folded when a workspace opens: GitKraken opens on LOCAL.
pub(super) fn default_collapsed() -> HashSet<PanelSection> {
    HashSet::from([
        PanelSection::Remote,
        PanelSection::Workspaces,
        PanelSection::Tags,
    ])
}

/// One line of a section's tree (see [`tree_lines`]).
#[derive(Debug, PartialEq)]
pub(super) enum TreeLine {
    /// A folder: `key` is its path joined with `/`, `depth` its level.
    Folder {
        key: String,
        name: String,
        depth: usize,
        open: bool,
    },
    /// The ref at `at` of the paths given, under `depth` folders.
    Ref {
        at: usize,
        name: String,
        depth: usize,
    },
}

/// The lines of the tree of `paths` (each a ref's path, in tree order), as
/// GitKraken folds `release/2.0.0` under `release`: a folder once, before
/// its first ref, and what is under a folder only while `is_open(key,
/// depth)` holds for it and every folder above it. `is_open` is asked once
/// per folder line, not per ref: a folded folder of a thousand refs costs one.
pub(super) fn tree_lines<S: AsRef<str> + PartialEq>(
    paths: &[&[S]],
    is_open: impl Fn(&str, usize) -> bool,
) -> Vec<TreeLine> {
    let mut lines = Vec::new();
    let mut previous: &[S] = &[];
    // whether each folder of `previous` is open, down to the first that is not
    let mut opens: Vec<bool> = Vec::new();
    for (at, path) in paths.iter().enumerate() {
        let Some((leaf, folders)) = path.split_last() else {
            continue;
        };
        let common = previous
            .iter()
            .zip(folders)
            .take_while(|(a, b)| a == b)
            .count();
        opens.truncate(common);
        // the folders shared with the previous ref are drawn already; under
        // a folded one there is nothing to draw
        let mut shown = opens.len() == common && opens.iter().all(|open| *open);
        if shown {
            for depth in common..folders.len() {
                let key = folders[..=depth]
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<&str>>()
                    .join("/");
                let open = is_open(&key, depth);
                lines.push(TreeLine::Folder {
                    name: folders[depth].as_ref().to_owned(),
                    key,
                    depth,
                    open,
                });
                opens.push(open);
                if !open {
                    shown = false;
                    break;
                }
            }
        }
        if shown {
            lines.push(TreeLine::Ref {
                at,
                name: leaf.as_ref().to_owned(),
                depth: folders.len(),
            });
        }
        previous = folders;
    }
    lines
}

/// The left padding of a panel line `depth` folders down.
fn indent(depth: usize) -> gpui_kit::Rems {
    gpui_kit::rems(1.25 + depth as f32)
}

impl RepoView {
    /// Asks for every revision with a bookmark, tag or working copy, for the
    /// panel, which lists nothing until it first answers.
    pub(super) fn load_panel_refs(&mut self, cx: &mut Context<Self>) {
        let Some(this_workspace) = self.workspace.as_ref().map(|ws| ws.name.clone()) else {
            return;
        };
        self.panel.generation += 1;
        let generation = self.panel.generation;
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    worker.query_headers(PANEL_REVSET.to_owned(), PANEL_LIMIT)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.panel.generation != generation {
                    return;
                }
                match result {
                    Ok(headers) => {
                        (view.panel.refs, view.panel.workspaces) =
                            panel_items(&headers, &this_workspace);
                    }
                    // the items of the last answer stay, placed in the new rows
                    Err(err) => log::warn!("side panel query failed: {err:#}"),
                }
                view.place_panel_items();
                cx.notify();
            });
        })
        .detach();
    }

    /// The panel's items placed in the rows again: new rows (their row
    /// numbers) or a new current bookmark. Their names stay as they are.
    pub(super) fn place_panel_items(&mut self) {
        place_panel_items(
            &mut self.panel.refs,
            &mut self.panel.workspaces,
            &self.log.row_of_commit,
            self.current_bookmark.as_deref(),
        );
    }

    /// Selects revision `id`: its row, or, when the log does not have it, the
    /// log widened to take it in (the revset field shows how).
    fn select_or_reveal(
        &mut self,
        id: &RevId,
        row: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = row {
            self.select(row, cx);
            return;
        }
        let Some(query) = self.workspace.as_ref().map(|ws| ws.query.clone()) else {
            return;
        };
        self.pending_select = Some(id.change.hex.clone());
        self.set_query(format!("({query}) | {}", id.commit.hex), window, cx);
    }

    fn toggle_section(&mut self, section: PanelSection, cx: &mut Context<Self>) {
        if !self.panel.collapsed.remove(&section) {
            self.panel.collapsed.insert(section);
        }
        self.notify_pane(PaneId::Side, cx);
    }

    pub(super) fn render_refs_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let needle = self.panel.filter.read(cx).value().trim().to_lowercase();
        let filtering = !needle.is_empty();
        let (mut local, mut remote, mut tags) = (Vec::new(), Vec::new(), Vec::new());
        for (key, item) in self.panel.refs.iter().enumerate() {
            if filtering && !item.search.contains(&needle) {
                continue;
            }
            match item.chip.kind {
                RefKind::Local => local.push((key, item)),
                RefKind::Remote => remote.push((key, item)),
                RefKind::Tag => tags.push((key, item)),
            }
        }
        let workspaces: Vec<(usize, &WorkspaceItem)> = self
            .panel
            .workspaces
            .iter()
            .enumerate()
            .filter(|(_, item)| !filtering || item.name.to_lowercase().contains(&needle))
            .collect();
        let pulls = self.render_pulls(&needle, cx);
        let viewing = local.len() + remote.len() + tags.len() + workspaces.len();
        // a filter opens every section, so what matches is in view
        let open = |section| filtering || !self.panel.collapsed.contains(&section);

        let mut body = v_flex().pb_2();
        body = body.child(self.section_header(
            PanelSection::Local,
            IconName::Laptop,
            "Local",
            local.len(),
            open(PanelSection::Local),
            cx,
        ));
        if open(PanelSection::Local) {
            body = body.children(self.render_tree(PanelSection::Local, &local, filtering, cx));
        }
        body = body.child(self.section_header(
            PanelSection::Remote,
            IconName::Cloud,
            "Remote",
            remote.len(),
            open(PanelSection::Remote),
            cx,
        ));
        if open(PanelSection::Remote) {
            // by remote, each a folder, as GitKraken nests them
            body = body.children(self.render_tree(PanelSection::Remote, &remote, filtering, cx));
        }
        body = body.child(self.section_header(
            PanelSection::Workspaces,
            IconName::FolderGit2,
            "Workspaces",
            workspaces.len(),
            open(PanelSection::Workspaces),
            cx,
        ));
        if open(PanelSection::Workspaces) {
            body = body.children(
                workspaces
                    .iter()
                    .map(|(key, item)| self.render_workspace_row(*key, item, cx)),
            );
        }
        body = body.children(pulls);
        body = body.child(self.section_header(
            PanelSection::Tags,
            IconName::Tag,
            "Tags",
            tags.len(),
            open(PanelSection::Tags),
            cx,
        ));
        if open(PanelSection::Tags) {
            body = body.children(self.render_tree(PanelSection::Tags, &tags, filtering, cx));
        }

        let theme = cx.theme();
        v_flex()
            .size_full()
            .min_h_0()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h_10()
                    .px_4()
                    .gap_2()
                    .flex_none()
                    .child(div().text_sm().font_medium().child("Viewing"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.primary)
                            .child(viewing.to_string()),
                    ),
            )
            .child(
                div().px_3().pb_2().flex_none().child(
                    Input::new(&self.panel.filter)
                        .small()
                        .suffix(gpui_kit::component::Icon::new(IconName::Search).xsmall())
                        .cleanable(true),
                ),
            )
            .child(div().h_px().flex_none().bg(theme.border))
            .child(
                div()
                    .id("side-panel")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .into_any_element()
    }

    /// A section's header: fold chevron, icon, name and count.
    pub(super) fn section_header(
        &self,
        section: PanelSection,
        icon: IconName,
        label: &'static str,
        count: usize,
        open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        panel_header(
            ("section", section as usize),
            icon,
            label,
            Some(count),
            open,
            cx,
        )
        .on_click(cx.listener(move |view, _, _, cx| view.toggle_section(section, cx)))
        .into_any_element()
    }

    /// A section's refs as their tree: folders, and the refs under the open
    /// ones (all of them while the panel filters).
    fn render_tree(
        &self,
        section: PanelSection,
        items: &[(usize, &RefItem)],
        filtering: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let paths: Vec<&[SharedString]> = items.iter().map(|(_, item)| &item.path[..]).collect();
        // the folders the current bookmark is in, so it stays in view
        let current: HashSet<String> = items
            .iter()
            .filter(|(_, item)| item.chip.current)
            .flat_map(|(_, item)| {
                (1..item.path.len()).map(|end| {
                    item.path[..end]
                        .iter()
                        .map(SharedString::as_ref)
                        .collect::<Vec<&str>>()
                        .join("/")
                })
            })
            .collect();
        let lines = tree_lines(&paths, |key, depth| {
            filtering || self.folder_open(section, key, depth, current.contains(key))
        });
        lines
            .into_iter()
            .map(|line| match line {
                TreeLine::Folder {
                    key,
                    name,
                    depth,
                    open,
                } => self.render_folder_row(section, key, name, depth, open, cx),
                TreeLine::Ref { at, name, depth } => {
                    let (key, item) = items[at];
                    self.render_ref_row(key, item, name, depth, cx)
                }
            })
            .collect()
    }

    /// Whether folder `key` of `section` is open: a remote's own folder and
    /// one the current bookmark is in (`current`) are until folded, the
    /// other folders in a name are once unfolded.
    fn folder_open(&self, section: PanelSection, key: &str, depth: usize, current: bool) -> bool {
        let by_default = current || (section == PanelSection::Remote && depth == 0);
        let flipped = self
            .panel
            .folders
            .get(&section)
            .is_some_and(|folders| folders.contains(key));
        by_default != flipped
    }

    fn toggle_folder(&mut self, section: PanelSection, key: &str, cx: &mut Context<Self>) {
        let folders = self.panel.folders.entry(section).or_default();
        if !folders.remove(key) {
            folders.insert(key.to_owned());
        }
        self.notify_pane(PaneId::Side, cx);
    }

    /// A folder of the tree: a click folds or unfolds it.
    fn render_folder_row(
        &self,
        section: PanelSection,
        key: String,
        name: String,
        depth: usize,
        open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let icon = match (section, depth, open) {
            (PanelSection::Remote, 0, _) => IconName::Cloud,
            (_, _, true) => IconName::FolderOpen,
            (_, _, false) => IconName::Folder,
        };
        ListItem::new(SharedString::from(format!(
            "panel-folder-{}-{key}",
            section as usize
        )))
        .py_0p5()
        .pl(indent(depth))
        .pr_3()
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .text_sm()
                .child(
                    gpui_kit::component::Icon::new(icon)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(div().min_w_0().truncate().child(name)),
        )
        .on_click(
            cx.listener(move |view, event: &gpui_kit::ClickEvent, _, cx| {
                // a double click is two clicks, and would fold it back
                if event.click_count() < 2 {
                    view.toggle_folder(section, &key, cx);
                }
            }),
        )
        .into_any_element()
    }

    /// One bookmark or tag: a click selects its revision, a double click
    /// switches to it, the ⋮ shown on hover or a right click has its
    /// commands, and it drags onto a revision to move there. `name` is the
    /// last part of its name; the tooltip has all of it.
    fn render_ref_row(
        &self,
        key: usize,
        item: &RefItem,
        name: String,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let mark_unpushed = settings::get(cx).mark_unpushed;
        let icon = match item.chip.kind {
            RefKind::Local | RefKind::Remote => IconName::GitBranch,
            RefKind::Tag => IconName::Tag,
        };
        let selected = item.row.is_some() && item.row == self.selected;
        let (id, row) = (item.id.clone(), item.row);
        let (switch_name, local) = (item.chip.name.clone(), item.chip.kind == RefKind::Local);
        let switch_id = item.id.clone();
        let full_name = item.full_name.clone();
        let target = MenuRef {
            store: item.chip.store.clone(),
            name: item.chip.name.clone(),
            full_name: full_name.clone(),
            id: item.id.clone(),
        };
        let owner = cx.entity().downgrade();
        let focus = self.command_focus();
        let menu = {
            let (target, owner, focus) = (target.clone(), owner.clone(), focus.clone());
            move |menu, _: &mut Window, cx: &mut Context<PopupMenu>| {
                panel_ref_menu(menu, &target, &owner, focus.clone(), cx)
            }
        };
        ListItem::new(("panel-ref", key))
            .group("panel-ref")
            .py_0p5()
            .pl(indent(depth))
            .pr_1()
            .selected(selected)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .text_sm()
                    .child(
                        gpui_kit::component::Icon::new(icon)
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when(item.chip.current, |this| this.font_semibold())
                            .child(name),
                    )
                    .when(item.chip.current, |this| {
                        this.child(
                            gpui_kit::component::Icon::new(IconName::Check)
                                .xsmall()
                                .text_color(theme.foreground),
                        )
                    })
                    .when(mark_unpushed && item.chip.unpushed, |this| {
                        this.child(
                            gpui_kit::component::Icon::new(IconName::Upload)
                                .xsmall()
                                .text_color(theme.warning),
                        )
                    })
                    .child(div().flex_1())
                    // GitKraken's ⋮, there while the pointer is on the line
                    .child(
                        div()
                            .flex_none()
                            .invisible()
                            .group_hover("panel-ref", |this| this.visible())
                            .child(
                                Button::new(("panel-ref-more", key))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::EllipsisVertical)
                                    .tooltip("More")
                                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, {
                                        let menu = menu.clone();
                                        move |popup, window, cx| menu(popup, window, cx)
                                    }),
                            ),
                    ),
            )
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(full_name.clone()).build(window, cx)
            })
            .on_click(
                cx.listener(move |view, event: &gpui_kit::ClickEvent, window, cx| {
                    view.select_or_reveal(&id, row, window, cx);
                    // a double click switches to it, as GitKraken checks a branch out
                    if event.click_count() >= 2 {
                        view.switch_to(switch_name.clone(), local, switch_id.clone(), window, cx);
                    }
                }),
            )
            .on_drag(
                DraggedRef {
                    store: item.chip.store.clone(),
                    label: item.chip.name.clone(),
                },
                |dragged, _, _, cx| {
                    cx.new(|_| DragChip {
                        label: format!("Move {}", dragged.label).into(),
                        icon: IconName::Bookmark,
                    })
                },
            )
            .context_menu(move |popup, window, cx| menu(popup, window, cx))
            .into_any_element()
    }

    /// One workspace: a click selects its working copy.
    fn render_workspace_row(
        &self,
        key: usize,
        item: &WorkspaceItem,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (id, row, this) = (item.id.clone(), item.row, item.this);
        ListItem::new(("panel-workspace", key))
            .py_0p5()
            .pl(gpui_kit::rems(2.25))
            .pr_3()
            .selected(row.is_some() && row == self.selected)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .text_sm()
                    .child(
                        gpui_kit::component::Icon::new(IconName::FolderGit2)
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when(item.this, |this| this.font_semibold())
                            .child(item.name.clone()),
                    )
                    .when(item.this, |this| {
                        this.child(
                            gpui_kit::component::Icon::new(IconName::Check)
                                .xsmall()
                                .text_color(theme.foreground),
                        )
                    }),
            )
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(if this {
                    "This tab's workspace"
                } else {
                    "Another workspace"
                })
                .build(window, cx)
            })
            .on_click(
                cx.listener(move |view, _, window, cx| view.select_or_reveal(&id, row, window, cx)),
            )
            .into_any_element()
    }
}

/// The ref a panel menu is for.
#[derive(Clone)]
struct MenuRef {
    store: StoreRef,
    /// As the log's chips name it (`main@origin` for a remote bookmark).
    name: SharedString,
    /// As git names it (`origin/main`).
    full_name: SharedString,
    id: RevId,
}

/// A ref's commands, after GitKraken's branch menu, in jj's terms: check out
/// (a new change on it), its bookmark commands, the names and ids to copy,
/// and delete. The bookmark commands act on the menu's ref, which opening
/// the menu sets.
fn panel_ref_menu(
    menu: PopupMenu,
    target: &MenuRef,
    owner: &WeakEntity<RepoView>,
    focus: FocusHandle,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    use actions::*;
    let _ = owner.update(cx, |view, _| view.ref_target = Some(target.store.clone()));
    let copy = |label: &str, text: String| {
        PopupMenuItem::new(label.to_owned()).on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
        })
    };
    let (name, id, owner) = (target.name.clone(), target.id.clone(), owner.clone());
    let local = matches!(target.store, StoreRef::LocalBookmark { .. });
    let check_out = PopupMenuItem::new(format!("Check out {}", target.full_name)).on_click(
        move |_, window, cx| {
            let _ = owner.update(cx, |view, cx| {
                view.switch_to(name.clone(), local, id.clone(), window, cx)
            });
        },
    );
    let menu = menu.action_context(focus).item(check_out);
    let menu = match &target.store {
        StoreRef::LocalBookmark { .. } => menu
            .separator()
            .menu("Push", Box::new(PushBookmark))
            .menu("Move to…", Box::new(PickMoveBookmark))
            .menu("Rename…", Box::new(RenameBookmark)),
        StoreRef::RemoteBookmark { is_tracked, .. } => menu.separator().menu(
            if *is_tracked { "Untrack" } else { "Track" },
            Box::new(ToggleTrackBookmark),
        ),
        StoreRef::Tag { .. } => menu,
    };
    menu.separator()
        .item(copy(
            match target.store {
                StoreRef::Tag { .. } => "Copy tag name",
                _ => "Copy bookmark name",
            },
            target.full_name.to_string(),
        ))
        .item(copy("Copy commit ID", target.id.commit.hex.clone()))
        .item(copy("Copy change ID", target.id.change.hex.clone()))
        .separator()
        .menu(format!("Delete {}", target.full_name), Box::new(DeleteRef))
}

/// A section header as GitKraken draws one: chevron, icon, the name in
/// capitals, and the count at the right (none while it is unknown).
pub(super) fn panel_header(
    id: impl Into<gpui_kit::ElementId>,
    icon: IconName,
    label: &'static str,
    count: Option<usize>,
    open: bool,
    cx: &App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.theme();
    let hover = theme.muted;
    h_flex()
        .id(id)
        .h_10()
        .px_3()
        .gap_2()
        .flex_none()
        .border_t_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(
            gpui_kit::component::Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall()
            .text_color(theme.muted_foreground),
        )
        .child(
            gpui_kit::component::Icon::new(icon)
                .small()
                .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(if open {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .child(label.to_uppercase()),
        )
        .child(div().flex_1())
        .when_some(count, |this, count| {
            this.child(
                div()
                    .text_sm()
                    .text_color(theme.primary)
                    .child(count.to_string()),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{TreeLine, tree_lines};

    fn folder(key: &str, depth: usize, open: bool) -> TreeLine {
        TreeLine::Folder {
            key: key.to_owned(),
            name: key.rsplit('/').next().unwrap_or(key).to_owned(),
            depth,
            open,
        }
    }

    fn leaf(at: usize, name: &str, depth: usize) -> TreeLine {
        TreeLine::Ref {
            at,
            name: name.to_owned(),
            depth,
        }
    }

    #[test]
    fn names_fold_into_folders_at_their_slashes() {
        let names = [
            "main",
            "release/1.3.4",
            "release/2.0.0",
            "van/dev/x",
            "van/qa",
        ];
        let split: Vec<Vec<&str>> = names.iter().map(|name| name.split('/').collect()).collect();
        let paths: Vec<&[&str]> = split.iter().map(Vec::as_slice).collect();

        // all open: each folder once, before its refs
        assert_eq!(
            tree_lines(&paths, |_, _| true),
            vec![
                leaf(0, "main", 0),
                folder("release", 0, true),
                leaf(1, "1.3.4", 1),
                leaf(2, "2.0.0", 1),
                folder("van", 0, true),
                folder("van/dev", 1, true),
                leaf(3, "x", 2),
                leaf(4, "qa", 1),
            ]
        );

        // a folded folder hides what is under it, folders under it too
        assert_eq!(
            tree_lines(&paths, |key, _| key != "release" && key != "van"),
            vec![
                leaf(0, "main", 0),
                folder("release", 0, false),
                folder("van", 0, false),
            ]
        );

        // a folded folder inside an open one, and what follows it
        assert_eq!(
            tree_lines(&paths, |key, _| key != "van/dev"),
            vec![
                leaf(0, "main", 0),
                folder("release", 0, true),
                leaf(1, "1.3.4", 1),
                leaf(2, "2.0.0", 1),
                folder("van", 0, true),
                folder("van/dev", 1, false),
                leaf(4, "qa", 1),
            ]
        );
    }

    #[test]
    fn a_folded_folder_is_looked_at_once() {
        let split: Vec<Vec<String>> = (0..1000)
            .map(|n| vec!["team".to_owned(), format!("b{n}")])
            .collect();
        let paths: Vec<&[String]> = split.iter().map(Vec::as_slice).collect();
        let asked = std::cell::Cell::new(0);
        let lines = tree_lines(&paths, |_, _| {
            asked.set(asked.get() + 1);
            false
        });
        assert_eq!(lines, vec![folder("team", 0, false)]);
        assert_eq!(asked.get(), 1);
    }
}
