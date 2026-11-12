//! The toolbar's workspace and bookmark pickers, GitKraken's repository and
//! branch switchers, and the conflict count beside them.
//!
//! jj has no current branch: the picker names the bookmark nearest to the
//! working copy, and choosing one starts a new change on it (`jj new
//! <bookmark>`), the closest jj has to a checkout.

use gpui_kit::component::{
    WindowExt as _,
    command::{Command, CommandItem, CommandState},
    notification::Notification,
};

use super::*;
use crate::app_view::OpenTabs;
use std::path::Path;

/// Conflicts the toolbar counts: the ones that can still be resolved.
const CONFLICT_REVSET: &str = "conflicts() & mutable()";
const CONFLICT_LIMIT: usize = 100;

/// What the bookmark picker lists.
const BOOKMARK_REVSET: &str = "bookmarks() | remote_bookmarks()";
const BOOKMARK_LIMIT: usize = 2000;

/// jj repositories in Settings' source folders, found in the background.
#[derive(Default)]
pub struct SourceRepos {
    repos: Vec<PathBuf>,
}

impl gpui_kit::Global for SourceRepos {}

/// How deep below a source folder repositories are looked for.
const SOURCE_DEPTH: usize = 2;

/// Looks for the repositories of Settings' source folders off the UI
/// thread; the workspace picker lists them. Run when the folders change and
/// whenever the picker opens, so repositories made since show up.
pub fn scan_source_folders(cx: &mut App) {
    let folders = settings::get(cx).display.source_folders.clone();
    if folders.is_empty() {
        cx.set_global(SourceRepos::default());
        return;
    }
    cx.spawn(async move |cx| {
        let search = folders.clone();
        let repos = cx
            .background_spawn(async move { find_repositories(&search) })
            .await;
        cx.update(|cx| {
            // a change of the folders meanwhile has a search of its own
            if settings::get(cx).display.source_folders == folders {
                cx.set_global(SourceRepos { repos });
            }
        });
    })
    .detach();
}

/// The jj workspaces in `folders` and up to [`SOURCE_DEPTH`] levels below,
/// not looking inside one (or inside hidden and build folders).
fn find_repositories(folders: &[String]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
        if dir.join(".jj").is_dir() {
            found.push(dir.to_owned());
            return;
        }
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && !matches!(name.as_ref(), "node_modules" | "target")
            })
            .map(|entry| entry.path())
            .collect();
        dirs.sort();
        for dir in dirs {
            walk(&dir, depth - 1, found);
        }
    }
    let mut found = Vec::new();
    for folder in folders {
        walk(Path::new(folder), SOURCE_DEPTH, &mut found);
    }
    found.dedup();
    found
}

/// One entry of the bookmark picker.
struct BookmarkChoice {
    name: SharedString,
    remote: bool,
    rev: RevHeader,
}

impl RepoView {
    /// The local bookmark nearest to the working copy (see
    /// [`nearest_bookmark`]).
    pub(super) fn current_bookmark(&self) -> Option<SharedString> {
        self.current_bookmark.clone()
    }

    /// Starts a new change on a ref's revision, `jj new <name>`: the closest
    /// jj has to a checkout. A local bookmark switched to is then the one
    /// marked current where others share its revision. Where the working
    /// copy already is a new change on that revision, only the mark moves.
    pub(super) fn switch_to(
        &mut self,
        name: SharedString,
        local: bool,
        id: RevId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.switched_to = local.then(|| name.to_string());
        if self.is_new_change_on(&id) {
            self.rebuild_derived();
            cx.notify();
            return;
        }
        self.check_picked_up_after();
        self.run_mutation(
            format!("New change on {name}"),
            Box::new(gg_lib::messages::mutations::CreateRevision {
                set: RevSet {
                    from: id.clone(),
                    to: id,
                },
            }),
            None,
            window,
            cx,
        )
    }

    /// The working copy is an empty, undescribed change on `id` alone: what
    /// `jj new` on it would make again.
    fn is_new_change_on(&self, id: &RevId) -> bool {
        !self.working_copy_changed
            && self.log.rows.iter().any(|row| {
                let rev = &row.revision;
                is_this_working_copy(rev)
                    && rev
                        .description
                        .lines
                        .iter()
                        .all(|line| line.trim().is_empty())
                    && matches!(rev.parent_ids.as_slice(), [parent] if parent.hex == id.commit.hex)
            })
    }

    /// Asks for every revision with a conflict that can still be resolved,
    /// loaded or not, after the log changed.
    pub(super) fn refresh_conflicts(&mut self, cx: &mut Context<Self>) {
        let worker = self.worker.clone();
        self.conflicts.loading = true;
        self.conflicts.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    worker.query_headers(CONFLICT_REVSET.to_owned(), CONFLICT_LIMIT)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.conflicts.loading = false;
                match result {
                    Ok(headers) => view.conflicts.revisions = headers,
                    Err(err) => log::warn!("conflict query failed: {err:#}"),
                }
                cx.notify();
            });
        }));
    }

    /// Selects the next conflicted revision below the selection, wrapping. One
    /// that is not loaded is shown with "All branches".
    pub(super) fn select_next_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.conflicts.revisions.is_empty() {
            return;
        }
        self.close_focus(window, cx);
        let row_of = |hex: &str| {
            self.log
                .rows
                .iter()
                .position(|row| row.revision.id.commit.hex == hex)
        };
        let after = self.selected.unwrap_or(0);
        let loaded: Vec<usize> = self
            .conflicts
            .revisions
            .iter()
            .filter_map(|header| row_of(&header.id.commit.hex))
            .collect();
        let next_loaded = loaded
            .iter()
            .copied()
            .filter(|&ix| ix > after)
            .min()
            .or_else(|| loaded.iter().copied().min());
        // the next loaded one, unless some are outside the loaded rows and
        // the selection is already past the last loaded one
        let unloaded = self
            .conflicts
            .revisions
            .iter()
            .find(|header| row_of(&header.id.commit.hex).is_none());
        match (next_loaded, unloaded) {
            (Some(ix), None) => self.select(ix, cx),
            (Some(ix), Some(_)) if ix > after => self.select(ix, cx),
            (_, Some(header)) if self.log_scope() != Some(true) => {
                self.pending_commit_select = Some(header.id.commit.hex.clone());
                self.set_log_scope(true, window, cx);
            }
            (_, Some(header)) => {
                // in every branch, but past the pages read so far
                let change = header.id.change.prefix.clone();
                window.push_notification(
                    Notification::info("Scroll down to load more revisions.")
                        .title(format!("Conflict in {change} is further down")),
                    cx,
                );
            }
            (None, None) => {}
        }
        self.log_focus.focus(window, cx);
    }

    /// Open tabs, then recent workspaces, then "Open…".
    pub(super) fn open_workspace_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let here = self.wd.clone();
        let open: Vec<(PathBuf, SharedString)> = cx
            .try_global::<OpenTabs>()
            .map(|tabs| tabs.0.clone())
            .unwrap_or_default();
        let recent: Vec<PathBuf> = settings::get(cx)
            .recent
            .iter()
            .map(PathBuf::from)
            // a moved or deleted workspace would only fail to open
            .filter(|path| path.join(".jj").is_dir())
            .filter(|path| !open.iter().any(|(open, _)| open == path))
            .collect();
        // Settings' source folders, as found last; looked for again now, so
        // repositories made since show up next time
        let in_folders: Vec<PathBuf> = cx
            .try_global::<SourceRepos>()
            .map(|found| found.repos.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|path| !open.iter().any(|(open, _)| open == path))
            .filter(|path| !recent.contains(path))
            .collect();
        scan_source_folders(cx);

        // (label, detail, path); `None` is "Open…"
        let mut entries: Vec<(SharedString, SharedString, Option<PathBuf>)> = open
            .into_iter()
            .map(|(path, title)| {
                let current = here.as_deref().is_some_and(|wd| {
                    std::fs::canonicalize(wd).unwrap_or(wd.to_path_buf()) == path
                });
                let detail = if current {
                    format!("{} · this tab", display_path(&path.to_string_lossy()))
                } else {
                    format!("{} · open", display_path(&path.to_string_lossy()))
                };
                (title, detail.into(), Some(path))
            })
            .collect();
        entries.extend(recent.into_iter().map(|path| {
            let title = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            (
                title.into(),
                format!("{} · recent", display_path(&path.to_string_lossy())).into(),
                Some(path),
            )
        }));
        entries.extend(in_folders.into_iter().map(|path| {
            let title = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            (
                title.into(),
                format!("{} · source folder", display_path(&path.to_string_lossy())).into(),
                Some(path),
            )
        }));
        entries.push(("Open…".into(), "Choose a folder".into(), None));

        let items = entries
            .iter()
            .map(|(label, detail, path)| {
                picker_item(
                    label.clone(),
                    detail.clone(),
                    if path.is_some() {
                        IconName::FolderGit2
                    } else {
                        IconName::FolderOpen
                    },
                )
            })
            .collect();
        let view = cx.entity().downgrade();
        let paths: Rc<Vec<Option<PathBuf>>> =
            Rc::new(entries.into_iter().map(|(_, _, path)| path).collect());
        open_picker(
            window,
            cx,
            "Switch workspace",
            items,
            move |ix, window, cx| match paths.get(ix).cloned() {
                Some(Some(path)) => {
                    let _ = view.update(cx, |_, cx| cx.emit(RepoViewEvent::OpenWorkspace(path)));
                }
                Some(None) => {
                    window.dispatch_action(Box::new(crate::app_view::actions::OpenRepository), cx)
                }
                None => {}
            },
        );
    }

    /// Lists every bookmark (local, then remote) and starts a new change on
    /// the one chosen.
    pub(super) fn open_bookmark_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let worker = self.worker.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    worker.query_headers(BOOKMARK_REVSET.to_owned(), BOOKMARK_LIMIT)
                })
                .await;
            let _ = this.update_in(cx, |view, window, cx| match result {
                Ok(headers) => view.show_bookmark_picker(bookmark_choices(headers), window, cx),
                Err(err) => window.push_notification(
                    Notification::error(format!("{err:#}")).title("Could not list the bookmarks"),
                    cx,
                ),
            });
        })
        .detach();
    }

    fn show_bookmark_picker(
        &mut self,
        choices: Vec<BookmarkChoice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.current_bookmark();
        let items = choices
            .iter()
            .map(|choice| {
                let summary = choice
                    .rev
                    .description
                    .lines
                    .first()
                    .cloned()
                    .unwrap_or_default();
                let here = current.as_ref() == Some(&choice.name);
                let detail = format!(
                    "{}{} {}",
                    if here { "current · " } else { "" },
                    choice.rev.id.change.prefix,
                    summary
                );
                picker_item(
                    choice.name.clone(),
                    detail.into(),
                    if choice.remote {
                        IconName::CloudDownload
                    } else {
                        IconName::GitBranch
                    },
                )
            })
            .collect();
        let view = cx.entity().downgrade();
        let choices = Rc::new(choices);
        open_picker(
            window,
            cx,
            "Start a new change on a bookmark",
            items,
            move |ix, window, cx| {
                let Some(choice) = choices.get(ix) else {
                    return;
                };
                let (name, local, id) =
                    (choice.name.clone(), !choice.remote, choice.rev.id.clone());
                let _ = view.update(cx, |view, cx| {
                    view.close_focus(window, cx);
                    view.switch_to(name, local, id, window, cx)
                });
            },
        );
    }

    /// The workspace and bookmark buttons and the conflict count, at the left
    /// of the toolbar.
    pub(super) fn render_switchers(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let title: SharedString = self
            .wd
            .as_ref()
            .and_then(|wd| wd.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".to_owned())
            .into();
        let path = self
            .workspace
            .as_ref()
            .map(|ws| ws.path.clone())
            .unwrap_or_default();
        let ready = matches!(self.state, LoadState::Ready);
        let bookmark = self.current_bookmark();
        let conflicts = self.conflicts.revisions.len();
        h_flex()
            .min_w_0()
            .gap_1()
            .child(
                Button::new("workspace-picker")
                    .ghost()
                    .small()
                    .icon(IconName::FolderGit2)
                    .label(title)
                    .tooltip(SharedString::from(format!("{path} · switch workspace")))
                    .on_click(
                        cx.listener(|view, _, window, cx| view.open_workspace_picker(window, cx)),
                    ),
            )
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(gpui_kit::component::Icon::new(IconName::ChevronRight).xsmall()),
            )
            .child(
                Button::new("bookmark-picker")
                    .ghost()
                    .small()
                    .icon(IconName::GitBranch)
                    .label(bookmark.unwrap_or_else(|| "no bookmark".into()))
                    .disabled(!ready)
                    .tooltip("Current bookmark · switch to another")
                    .on_click(
                        cx.listener(|view, _, window, cx| view.open_bookmark_picker(window, cx)),
                    ),
            )
            .when(conflicts > 0, |this| {
                this.child(
                    Button::new("conflicts")
                        .ghost()
                        .small()
                        .icon(IconName::TriangleAlert)
                        .label(if conflicts >= CONFLICT_LIMIT {
                            format!("{CONFLICT_LIMIT}+ conflicts")
                        } else {
                            format!(
                                "{conflicts} conflict{}",
                                if conflicts == 1 { "" } else { "s" }
                            )
                        })
                        .text_color(theme.danger)
                        .tooltip("Conflicts · go to the next")
                        .on_click(
                            cx.listener(|view, _, window, cx| {
                                view.select_next_conflict(window, cx)
                            }),
                        ),
                )
            })
            .into_any_element()
    }
}

/// Bookmarks of `headers`: local ones first, then remote, each by name.
fn bookmark_choices(headers: Vec<RevHeader>) -> Vec<BookmarkChoice> {
    let mut choices: Vec<BookmarkChoice> = headers
        .iter()
        .flat_map(|header| {
            header.refs.iter().filter_map(move |r| match r {
                StoreRef::LocalBookmark { bookmark_name, .. } => Some(BookmarkChoice {
                    name: bookmark_name.clone().into(),
                    remote: false,
                    rev: header.clone(),
                }),
                StoreRef::RemoteBookmark {
                    bookmark_name,
                    remote_name,
                    ..
                } => Some(BookmarkChoice {
                    name: format!("{bookmark_name}@{remote_name}").into(),
                    remote: true,
                    rev: header.clone(),
                }),
                StoreRef::Tag { .. } => None,
            })
        })
        .collect();
    choices.sort_by(|a, b| a.remote.cmp(&b.remote).then_with(|| a.name.cmp(&b.name)));
    choices.dedup_by(|a, b| a.name == b.name);
    choices
}

/// A picker row: the name, searchable, over a muted detail line.
fn picker_item(label: SharedString, detail: SharedString, icon: IconName) -> CommandItem {
    let (name, line) = (label.clone(), detail.clone());
    CommandItem::new()
        .label(label)
        .keywords([detail])
        .child(move |_, cx| {
            let theme = cx.theme();
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(
                    gpui_kit::component::Icon::new(icon)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().truncate().child(name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(line.clone()),
                        ),
                )
        })
}

/// A searchable list in a dialog, like the command palette; `confirm` gets
/// the chosen row.
fn open_picker(
    window: &mut Window,
    cx: &mut Context<RepoView>,
    placeholder: &'static str,
    items: Vec<CommandItem>,
    confirm: impl Fn(usize, &mut Window, &mut App) + 'static,
) {
    let state = cx.new(|cx| CommandState::new(window, cx));
    {
        let state = state.clone();
        window.defer(cx, move |window, cx| {
            state.update(cx, |state, cx| state.focus(window, cx));
        });
    }
    let confirm = Rc::new(confirm);
    window.open_dialog(cx, move |dialog, _, _| {
        let confirm = confirm.clone();
        dialog.p_0().w(gpui_kit::px(560.)).child(
            Command::new(&state)
                .bordered(false)
                .placeholder(placeholder)
                .max_h(gpui_kit::px(420.))
                .items(items.clone())
                .on_confirm(move |index, window, cx| {
                    window.close_dialog(cx);
                    confirm(index.row, window, cx);
                }),
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_folders_find_repositories_two_levels_down() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for repo in [
            "app/.jj",
            "group/lib/.jj",
            "group/deep/too/.jj",
            ".hidden/repo/.jj",
            "target/repo/.jj",
            "app/nested/.jj",
        ] {
            std::fs::create_dir_all(root.join(repo)).unwrap();
        }
        let found = find_repositories(&[root.display().to_string()]);
        let names: Vec<String> = found
            .iter()
            .map(|path| path.strip_prefix(root).unwrap().display().to_string())
            .collect();
        // not three levels down, not in hidden or build folders, not inside
        // a repository
        assert_eq!(names, vec!["app", "group/lib"]);
    }
}
