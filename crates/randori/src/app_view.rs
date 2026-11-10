//! The window: a tab per jj workspace, like Fork or GitKraken.
//!
//! Each tab owns a [`RepoView`] and, through it, its own jj worker thread
//! (jj-lib is not thread-safe, so workspaces never share one). The open tabs
//! are saved and restored on the next launch.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _, Theme, ThemeMode,
    TitleBar, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    command::{Command, CommandItem, CommandState},
    h_flex,
    menu::{DropdownMenu as _, PopupMenu},
    notification::Notification,
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::{
    Action, Anchor, AppContext as _, Context, Entity, FocusHandle, Image, ImageFormat,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, PathPromptOptions,
    Render, SharedString, Styled as _, Subscription, Window, div, img, prelude::FluentBuilder as _,
    px,
};
use serde::{Deserialize, Serialize};

use crate::repo_view::{RepoView, RepoViewEvent};
use crate::settings::{self, DiffLayout, DisplaySettings, PULL_REFRESH_CHOICES, Settings};
use crate::worker::Worker;

const LOGO: &[u8] = include_bytes!("../assets/icon/randori-128.png");
const APP_CONTEXT: &str = "RandoriApp";

pub mod actions {
    gpui_kit::actions!(
        randori,
        [
            OpenRepository,
            CloseTab,
            NextTab,
            PreviousTab,
            OpenCommandPalette,
            ThemeLight,
            ThemeDark,
            ThemeSystem,
            Quit,
            Hide,
            HideOthers,
            ShowAll,
        ]
    );
}

/// Binds the tab keys. Call once at startup.
pub fn bind_keys(cx: &mut gpui_kit::App) {
    use actions::*;
    // `cmd-` on macOS, `ctrl-` elsewhere.
    let (open, close, palette) = if cfg!(target_os = "macos") {
        ("cmd-o", "cmd-w", "cmd-p")
    } else {
        ("ctrl-o", "ctrl-w", "ctrl-p")
    };
    cx.bind_keys([
        KeyBinding::new(open, OpenRepository, Some(APP_CONTEXT)),
        KeyBinding::new(close, CloseTab, Some(APP_CONTEXT)),
        KeyBinding::new(palette, OpenCommandPalette, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-tab", NextTab, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some(APP_CONTEXT)),
    ]);
    // global: they work with nothing focused, and from the menu bar
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
        ]);
    } else {
        cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);
    }
}

struct RepoTab {
    path: PathBuf,
    title: SharedString,
    view: Entity<RepoView>,
    /// The workspace did not open (not a jj repo, missing, broken); such tabs
    /// stay open to show why, but are not saved for the next launch.
    failed: bool,
    _events: Subscription,
}

/// Tabs restored on the next launch.
#[derive(Serialize, Deserialize, Default)]
struct Session {
    tabs: Vec<PathBuf>,
    active: usize,
    /// Appearance chosen in Settings; absent means the dark default.
    #[serde(default)]
    theme: ThemeChoice,
    /// "All branches" was the last scope picked; new tabs open with it.
    #[serde(default)]
    all_branches: bool,
    /// Diff layout and GitHub; the `gg.*` settings live in jj's config.
    #[serde(default)]
    settings: DisplaySettings,
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    Light,
    #[default]
    Dark,
    System,
}

pub struct RandoriApp {
    tabs: Vec<RepoTab>,
    active: usize,
    logo: Arc<Image>,
    /// Whether open tabs are saved and restored (off for screenshots).
    persist: bool,
    /// Appearance shown now; `--light` can override it for one run.
    theme: ThemeChoice,
    /// Appearance chosen in Settings, which is what the session remembers.
    saved_theme: ThemeChoice,
    /// Log scope new tabs start with (the last one picked).
    all_branches: bool,
    /// Focus for the window itself, so its keys (⌘P, ⌘O) work with no tab open.
    focus: FocusHandle,
    _activation: Subscription,
    _appearance: Subscription,
    /// Display settings as last saved, so the session can be written without `cx`.
    display: DisplaySettings,
    _settings: Subscription,
}

impl RandoriApp {
    /// Restores the previous session (when `persist`), then opens `paths` and
    /// selects the first of them. With nothing to open, falls back to the
    /// current directory if it is a jj workspace.
    pub fn new(
        paths: Vec<PathBuf>,
        revset: Option<String>,
        persist: bool,
        theme_override: Option<ThemeChoice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Pick up edits made outside Randori when the window comes back to the
        // front. Only the selected tab snapshots; the others do when selected.
        let _activation = cx.observe_window_activation(window, |app, window, cx| {
            if window.is_window_active() {
                app.snapshot_active(cx);
            }
        });
        // "Follow system" re-applies the theme when macOS / the desktop switches.
        let _appearance = cx.observe_window_appearance(window, |app, window, cx| {
            if app.theme == ThemeChoice::System {
                Theme::sync_system_appearance(Some(window), cx);
            }
        });
        // The close button asks first when descriptions would be lost; closing
        // the last window quits (see `main`).
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |app, cx| {
                if app.unsaved_descriptions(cx) == 0 {
                    app.save_session();
                    true
                } else {
                    app.request_quit(window, cx);
                    false
                }
            })
            .unwrap_or(true)
        });
        let session = if persist {
            load_session()
        } else {
            Session::default()
        };
        // before any tab opens: views read it when they are created
        cx.set_global(Settings::load(session.settings.clone()));
        let _settings = cx.observe_global::<Settings>(|app: &mut Self, cx| {
            app.display = settings::get(cx).display.clone();
            app.save_session();
            cx.notify();
        });
        let mut app = Self {
            tabs: Vec::new(),
            active: 0,
            logo: Arc::new(Image::from_bytes(ImageFormat::Png, LOGO.to_vec())),
            persist,
            theme: theme_override.unwrap_or(session.theme),
            saved_theme: session.theme,
            all_branches: session.all_branches,
            focus: cx.focus_handle(),
            _activation,
            _appearance,
            display: session.settings.clone(),
            _settings,
        };
        app.apply_theme(window, cx);
        // A workspace that was moved or deleted is dropped from the session
        // instead of coming back as an error tab on every launch.
        let (existing, missing) = split_existing(session.tabs);
        for path in existing {
            if let Err(err) = app.open_tab(path.clone(), None, window, cx) {
                log::warn!("cannot restore {}: {err:#}", path.display());
            }
        }
        if !missing.is_empty() {
            let list: Vec<String> = missing
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            window.push_notification(
                Notification::info(list.join("\n")).title("Workspaces no longer found"),
                cx,
            );
        }
        let mut active = session.active;

        let paths = if paths.is_empty() && app.tabs.is_empty() {
            gg_lib::config::resolve_repo_path(None)
                .into_iter()
                .collect()
        } else {
            paths
        };
        for (ix, path) in paths.into_iter().enumerate() {
            // The revset option applies to the first workspace on the command line.
            let revset = if ix == 0 { revset.clone() } else { None };
            match app.open_tab(path.clone(), revset, window, cx) {
                Ok(tab) if ix == 0 => active = tab,
                Ok(_) => {}
                Err(err) => log::warn!("cannot open {}: {err:#}", path.display()),
            }
        }
        app.active = active.min(app.tabs.len().saturating_sub(1));
        app.save_session();
        // opening each tab focused it in turn; give the keyboard to the selected one
        match app.tabs.get(app.active).map(|tab| tab.view.clone()) {
            Some(view) => view.update(cx, |view, cx| view.focus_log(window, cx)),
            None => app.focus.focus(window, cx),
        }
        app
    }

    /// The repository view of the selected tab.
    pub fn active_view(&self) -> Option<Entity<RepoView>> {
        self.tabs.get(self.active).map(|tab| tab.view.clone())
    }

    /// Opens a workspace in a new tab, or selects its tab if it is already open.
    /// Returns the tab index.
    fn open_tab(
        &mut self,
        path: PathBuf,
        revset: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<usize> {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        // A folder inside a workspace opens (or selects) the workspace itself.
        let path = workspace_root(&path).unwrap_or(path);
        if let Some(ix) = self.tabs.iter().position(|tab| tab.path == path) {
            self.activate(ix, window, cx);
            return Ok(ix);
        }

        // `--revset` wins; otherwise the last scope picked.
        let revset = revset.or_else(|| {
            self.all_branches
                .then(|| crate::repo_view::ALL_BRANCHES_QUERY.to_owned())
        });
        let repo_path = gg_lib::config::resolve_repo_path(Some(&path));
        let (settings, _, _, _) =
            gg_lib::read_config(repo_path.as_deref()).context("reading jj config")?;
        let (worker, events) = Worker::spawn(Some(path.clone()), settings)?;
        let view =
            cx.new(|cx| RepoView::new(worker, events, Some(path.clone()), revset, window, cx));
        let _events = cx.subscribe_in(
            &view,
            window,
            |app, view, event: &RepoViewEvent, window, cx| {
                match event {
                    RepoViewEvent::Loaded | RepoViewEvent::LoadFailed => {
                        let loaded = matches!(event, RepoViewEvent::Loaded);
                        let path = app
                            .tabs
                            .iter_mut()
                            .find(|tab| tab.view == *view)
                            .map(|tab| {
                                tab.failed = !loaded;
                                tab.path.clone()
                            });
                        // screenshots must not change the user's recent list
                        if let Some(path) = path.filter(|_| loaded && app.persist) {
                            settings::remember_workspace(cx, &path);
                        }
                    }
                    RepoViewEvent::ScopeChanged { all } => app.all_branches = *all,
                    RepoViewEvent::OpenWorkspace(path) => {
                        if let Err(err) = app.open_tab(path.clone(), None, window, cx) {
                            window.push_notification(
                                Notification::error(format!("{err:#}"))
                                    .title("Could not open the workspace"),
                                cx,
                            );
                        }
                    }
                }
                app.save_session();
            },
        );
        self.tabs.push(RepoTab {
            title: tab_title(&path),
            path,
            view,
            failed: false,
            _events,
        });
        let ix = self.tabs.len() - 1;
        self.publish_tabs(cx);
        self.activate(ix, window, cx);
        Ok(ix)
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let switched = self.active != ix;
        self.active = ix;
        let view = self.tabs[ix].view.clone();
        view.update(cx, |view, cx| {
            view.focus_log(window, cx);
            if switched {
                view.snapshot(cx);
            }
        });
        self.save_session();
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        // Dropping the view drops its worker handle; the worker thread then ends.
        self.tabs.remove(ix);
        self.publish_tabs(cx);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if ix < self.active {
            self.active -= 1;
        }
        if self.tabs.is_empty() {
            self.focus.focus(window, cx);
        } else {
            self.activate(self.active, window, cx);
        }
        self.save_session();
        cx.notify();
    }

    fn apply_theme(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.theme {
            ThemeChoice::Light => Theme::change(ThemeMode::Light, Some(window), cx),
            ThemeChoice::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
            ThemeChoice::System => Theme::sync_system_appearance(Some(window), cx),
        }
    }

    fn set_theme(&mut self, theme: ThemeChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.theme = theme;
        self.saved_theme = theme;
        self.apply_theme(window, cx);
        self.save_session();
        cx.notify();
    }

    /// ⌘P (as in GitKraken): every command of the window and the selected workspace, searchable.
    /// A confirmed item runs its action from the revision list, the same path a
    /// menu item or key binding takes.
    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::repo_view::actions as repo;
        let mut commands: Vec<(&'static str, IconName, Box<dyn Action>)> = Vec::new();
        if !self.tabs.is_empty() {
            commands.extend([
                (
                    "New change",
                    IconName::Plus,
                    Box::new(repo::NewChild) as Box<dyn Action>,
                ),
                (
                    "Edit revision",
                    IconName::Pencil,
                    Box::new(repo::EditRevision),
                ),
                (
                    "Describe revision",
                    IconName::TextCursorInput,
                    Box::new(repo::DescribeRevision),
                ),
                (
                    "Rebase selection onto…",
                    IconName::GitBranch,
                    Box::new(repo::PickRebase),
                ),
                (
                    "Move selected file to revision…",
                    IconName::FileInput,
                    Box::new(repo::PickMoveFile),
                ),
                (
                    "Create bookmark",
                    IconName::Bookmark,
                    Box::new(repo::CreateBookmark),
                ),
                (
                    "Abandon revision",
                    IconName::Trash,
                    Box::new(repo::AbandonRevision),
                ),
                (
                    "Duplicate revision",
                    IconName::Copy,
                    Box::new(repo::DuplicateRevision),
                ),
                (
                    "Back out revision",
                    IconName::Undo,
                    Box::new(repo::BackoutRevision),
                ),
                (
                    "Squash file into parent",
                    IconName::ArrowDownToLine,
                    Box::new(repo::SquashFile),
                ),
                (
                    "Discard file changes",
                    IconName::FileX,
                    Box::new(repo::RestoreFile),
                ),
                (
                    "Squash selected hunk into parent",
                    IconName::ArrowDownToLine,
                    Box::new(repo::SquashHunk),
                ),
                ("Push bookmarks", IconName::Upload, Box::new(repo::Push)),
                ("Fetch", IconName::CloudDownload, Box::new(repo::Fetch)),
                ("Undo", IconName::Undo2, Box::new(repo::Undo)),
                ("Redo", IconName::Redo2, Box::new(repo::Redo)),
                (
                    "Reload revisions",
                    IconName::RefreshCw,
                    Box::new(repo::Reload),
                ),
                ("Edit revset", IconName::Search, Box::new(repo::FocusRevset)),
                (
                    "Show my work (jj log)",
                    IconName::GitCommitHorizontal,
                    Box::new(repo::ShowMyWork),
                ),
                (
                    "Show all branches",
                    IconName::GitBranch,
                    Box::new(repo::ShowAllBranches),
                ),
                (
                    "Switch workspace…",
                    IconName::FolderGit2,
                    Box::new(repo::SwitchWorkspace),
                ),
                (
                    "Start a new change on a bookmark…",
                    IconName::GitBranch,
                    Box::new(repo::SwitchBookmark),
                ),
                (
                    "Go to the next conflict",
                    IconName::TriangleAlert,
                    Box::new(repo::NextConflict),
                ),
                (
                    "Open the file's full view",
                    IconName::Maximize2,
                    Box::new(repo::OpenFileDiff),
                ),
                (
                    "Blame the file",
                    IconName::FileText,
                    Box::new(repo::OpenFileBlame),
                ),
                (
                    "File history",
                    IconName::GitGraph,
                    Box::new(repo::OpenFileHistory),
                ),
                (
                    "Show the whole file",
                    IconName::FileText,
                    Box::new(repo::OpenFileView),
                ),
                (
                    "List revisions by date",
                    IconName::Clock,
                    Box::new(repo::OrderByDate),
                ),
                (
                    "Group revisions by branch",
                    IconName::GitBranch,
                    Box::new(repo::GroupByBranch),
                ),
                (
                    "Widen the original side of split diffs",
                    IconName::ChevronRight,
                    Box::new(repo::WidenOriginalSide),
                ),
                (
                    "Widen the modified side of split diffs",
                    IconName::ChevronLeft,
                    Box::new(repo::WidenModifiedSide),
                ),
                (
                    "Even the sides of split diffs",
                    IconName::Columns2,
                    Box::new(repo::EvenSplitSides),
                ),
                (
                    "Refresh pull requests",
                    IconName::RefreshCw,
                    Box::new(repo::RefreshPullRequests),
                ),
                (
                    "Show or hide bookmarks",
                    IconName::PanelLeft,
                    Box::new(repo::ToggleSidebar),
                ),
                (
                    "Show or hide the terminal",
                    IconName::SquareTerminal,
                    Box::new(repo::ToggleTerminal),
                ),
            ]);
        }
        commands.extend([
            (
                "Create a jj repository here",
                IconName::FolderGit2,
                Box::new(crate::repo_view::actions::InitRepository) as Box<dyn Action>,
            ),
            (
                "Open workspace",
                IconName::FolderOpen,
                Box::new(actions::OpenRepository) as Box<dyn Action>,
            ),
            ("Close tab", IconName::X, Box::new(actions::CloseTab)),
            (
                "Next tab",
                IconName::ChevronRight,
                Box::new(actions::NextTab),
            ),
            (
                "Previous tab",
                IconName::ChevronLeft,
                Box::new(actions::PreviousTab),
            ),
            ("Light theme", IconName::Sun, Box::new(actions::ThemeLight)),
            ("Dark theme", IconName::Moon, Box::new(actions::ThemeDark)),
            (
                "Follow system theme",
                IconName::Monitor,
                Box::new(actions::ThemeSystem),
            ),
        ]);

        let items: Vec<CommandItem> = commands
            .iter()
            .map(|(label, icon, _)| CommandItem::new().label(*label).icon(*icon))
            .collect();
        let actions: Rc<Vec<Box<dyn Action>>> =
            Rc::new(commands.into_iter().map(|(_, _, action)| action).collect());
        let palette = cx.new(|cx| CommandState::new(window, cx));
        let target = self.active_view();
        let app = cx.entity().downgrade();

        {
            let palette = palette.clone();
            window.defer(cx, move |window, cx| {
                palette.update(cx, |state, cx| state.focus(window, cx));
            });
        }
        window.open_dialog(cx, move |dialog, _, _| {
            let actions = actions.clone();
            let target = target.clone();
            let app = app.clone();
            dialog.p_0().w(px(560.)).child(
                Command::new(&palette)
                    .bordered(false)
                    .placeholder("Type a command")
                    .max_h(px(420.))
                    .items(items.clone())
                    .on_confirm(move |index, window, cx| {
                        let Some(action) = actions.get(index.row).map(|a| a.boxed_clone()) else {
                            return;
                        };
                        window.close_dialog(cx);
                        // Window commands run on the app itself: with no tab open there
                        // is no focused element for a dispatched action to start from.
                        if let Some(app) = app.upgrade()
                            && app.update(cx, |app, cx| {
                                app.run_window_action(action.as_ref(), window, cx)
                            })
                        {
                            return;
                        }
                        // Workspace commands run from the revision list, like a key binding.
                        if let Some(view) = &target {
                            view.update(cx, |view, cx| view.focus_log(window, cx));
                        }
                        window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
                    }),
            )
        });
    }

    /// Runs a window-level action directly. Returns false for workspace actions.
    fn run_window_action(
        &mut self,
        action: &dyn Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let any = action.as_any();
        if any.is::<actions::OpenRepository>() {
            self.prompt_open(window, cx);
        } else if any.is::<actions::CloseTab>() {
            self.request_close_tab(self.active, window, cx);
        } else if any.is::<actions::NextTab>() {
            self.cycle(1, window, cx);
        } else if any.is::<actions::PreviousTab>() {
            self.cycle(-1, window, cx);
        } else if any.is::<actions::ThemeLight>() {
            self.set_theme(ThemeChoice::Light, window, cx);
        } else if any.is::<actions::ThemeDark>() {
            self.set_theme(ThemeChoice::Dark, window, cx);
        } else if any.is::<actions::ThemeSystem>() {
            self.set_theme(ThemeChoice::System, window, cx);
        } else {
            return false;
        }
        true
    }

    /// Closes a tab, asking first when it holds descriptions that were not saved.
    /// Descriptions typed but not saved, across every tab.
    fn unsaved_descriptions(&self, cx: &gpui_kit::App) -> usize {
        self.tabs
            .iter()
            .map(|tab| tab.view.read(cx).unsaved_descriptions(cx))
            .sum()
    }

    /// Quits, after confirming when descriptions would be lost.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let unsaved = self.unsaved_descriptions(cx);
        if unsaved == 0 {
            self.save_session();
            cx.quit();
            return;
        }
        window.open_alert_dialog(cx, move |alert, _, _| {
            alert
                .title("Quit Randori?")
                .description(format!(
                    "{unsaved} unsaved description{} will be lost.",
                    if unsaved == 1 { "" } else { "s" }
                ))
                .confirm()
                .ok_text("Quit")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(|_, _, cx| {
                    cx.quit();
                    true
                })
        });
    }

    fn request_close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            // ⌘W with no tab left closes the window, which quits.
            if self.tabs.is_empty() {
                window.remove_window();
            }
            return;
        };
        let unsaved = tab.view.read(cx).unsaved_descriptions(cx);
        if unsaved == 0 {
            self.close_tab(ix, window, cx);
            return;
        }
        let title = tab.title.clone();
        let view = tab.view.clone();
        let app = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let app = app.clone();
            let view = view.clone();
            alert
                .title(format!("Close {title}?"))
                .description(format!(
                    "{unsaved} unsaved description{} will be lost.",
                    if unsaved == 1 { "" } else { "s" }
                ))
                .confirm()
                .ok_text("Close")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let view = view.clone();
                    let _ = app.update(cx, |app, cx| {
                        // The tab may have moved since the dialog opened.
                        if let Some(ix) = app.tabs.iter().position(|tab| tab.view == view) {
                            app.close_tab(ix, window, cx);
                        }
                    });
                    true
                })
        });
    }

    fn snapshot_active(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.active_view() {
            view.update(cx, |view, cx| view.snapshot(cx));
        }
    }

    fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        let len = self.tabs.len() as isize;
        let next = (self.active as isize + delta).rem_euclid(len) as usize;
        self.activate(next, window, cx);
    }

    fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let _ = this.update_in(cx, |app, window, cx| {
                for path in paths {
                    if let Err(err) = app.open_tab(path.clone(), None, window, cx) {
                        window.push_notification(
                            Notification::error(format!("{err:#}"))
                                .title(format!("Cannot open {}", path.display())),
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    /// Tells the tabs which workspaces are open, for their workspace picker.
    fn publish_tabs(&self, cx: &mut gpui_kit::App) {
        cx.set_global(OpenTabs(
            self.tabs
                .iter()
                .map(|tab| (tab.path.clone(), tab.title.clone()))
                .collect(),
        ));
    }

    fn save_session(&self) {
        if !self.persist {
            return;
        }
        let failed: Vec<bool> = self.tabs.iter().map(|tab| tab.failed).collect();
        let session = Session {
            tabs: self
                .tabs
                .iter()
                .filter(|tab| !tab.failed)
                .map(|tab| tab.path.clone())
                .collect(),
            active: saved_active(&failed, self.active),
            theme: self.saved_theme,
            all_branches: self.all_branches,
            settings: self.display.clone(),
        };
        if let Err(err) = store_session(&session) {
            log::warn!("cannot save open tabs: {err:#}");
        }
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .size_full()
            .min_w_0()
            .pr_2()
            .gap_1()
            .child(
                TabBar::new("repos")
                    .underline()
                    .small()
                    .min_w_0()
                    .selected_index(self.active)
                    .on_click(
                        cx.listener(|app, ix: &usize, window, cx| app.activate(*ix, window, cx)),
                    )
                    .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                        Tab::new()
                            .label(tab.title.clone())
                            .prefix(
                                Icon::new(IconName::FolderGit2)
                                    .xsmall()
                                    .text_color(theme.muted_foreground),
                            )
                            .suffix(
                                Button::new(("close-tab", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::X)
                                    .tooltip("Close tab")
                                    .on_click(cx.listener(move |app, _, window, cx| {
                                        cx.stop_propagation();
                                        app.request_close_tab(ix, window, cx);
                                    })),
                            )
                    })),
            )
            .child(
                Button::new("open-repo")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .tooltip("Open a workspace")
                    .on_click(cx.listener(|app, _, window, cx| app.prompt_open(window, cx))),
            )
            .child(div().flex_1())
    }

    fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .h_full()
            .w_12()
            .flex_none()
            .items_center()
            .py_3()
            .gap_2()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(img(self.logo.clone()).size_7().mb_2())
            .child(
                Button::new("rail-log")
                    .ghost()
                    .icon(IconName::GitGraph)
                    .selected(!self.tabs.is_empty())
                    .tooltip("Revisions"),
            )
            .child(
                Button::new("rail-open")
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .tooltip("Open a workspace")
                    .on_click(cx.listener(|app, _, window, cx| app.prompt_open(window, cx))),
            )
            .child(
                Button::new("rail-palette")
                    .ghost()
                    .icon(IconName::Command)
                    .tooltip("Command palette (⌘P)")
                    .on_click(cx.listener(|app, _, window, cx| app.open_palette(window, cx))),
            )
            .child(div().flex_1())
            .child({
                let theme = self.theme;
                Button::new("rail-settings")
                    .ghost()
                    .icon(IconName::Settings)
                    .tooltip("Settings")
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, cx| {
                        settings_menu(menu, theme, cx)
                    })
            })
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(img(self.logo.clone()).size_16())
            .child(div().text_lg().font_medium().child("Open a jj workspace"))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Each workspace opens in its own tab and is restored next time."),
            )
            .child(
                Button::new("empty-open")
                    .primary()
                    .label("Open…")
                    .on_click(cx.listener(|app, _, window, cx| app.prompt_open(window, cx))),
            )
    }
}

impl Render for RandoriApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("randori")
            .key_context(APP_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|app, _: &actions::OpenRepository, window, cx| {
                app.prompt_open(window, cx)
            }))
            .on_action(
                cx.listener(|app, _: &actions::Quit, window, cx| app.request_quit(window, cx)),
            )
            .on_action(cx.listener(|app, _: &actions::CloseTab, window, cx| {
                app.request_close_tab(app.active, window, cx)
            }))
            .on_action(
                cx.listener(|app, _: &actions::NextTab, window, cx| app.cycle(1, window, cx)),
            )
            .on_action(
                cx.listener(|app, _: &actions::OpenCommandPalette, window, cx| {
                    app.open_palette(window, cx)
                }),
            )
            .on_action(cx.listener(|app, _: &actions::ThemeLight, window, cx| {
                app.set_theme(ThemeChoice::Light, window, cx)
            }))
            .on_action(cx.listener(|app, _: &actions::ThemeDark, window, cx| {
                app.set_theme(ThemeChoice::Dark, window, cx)
            }))
            .on_action(cx.listener(|app, _: &actions::ThemeSystem, window, cx| {
                app.set_theme(ThemeChoice::System, window, cx)
            }))
            .on_action(
                cx.listener(|app, _: &actions::PreviousTab, window, cx| app.cycle(-1, window, cx)),
            )
            .child(TitleBar::new().child(self.render_tabs(cx)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_rail(cx))
                    .child(div().flex_1().min_w_0().h_full().flex().map(|this| {
                        match self.tabs.get(self.active) {
                            Some(tab) => this.child(tab.view.clone()),
                            None => this.child(self.render_empty(cx)),
                        }
                    })),
            )
    }
}

/// Workspaces open in tabs, as `(root, title)`: the toolbar's workspace
/// picker lists them.
pub struct OpenTabs(pub Vec<(PathBuf, SharedString)>);

impl gpui_kit::Global for OpenTabs {}

/// The Settings menu on the rail. Display settings apply at once; the jj ones
/// are written to the user's jj config (the same keys GG and `jj` read).
fn settings_menu(menu: PopupMenu, theme: ThemeChoice, cx: &gpui_kit::App) -> PopupMenu {
    use gpui_kit::component::menu::PopupMenuItem;
    let current = settings::get(cx);
    let display = current.display.clone();
    let auto_snapshot = current.auto_snapshot;
    let mark_unpushed = current.mark_unpushed;

    // A jj config write can fail (no writable config); say so instead of
    // pretending the setting changed.
    fn report(result: anyhow::Result<()>, window: &mut Window, cx: &mut gpui_kit::App) {
        if let Err(err) = result {
            window.push_notification(
                Notification::error(format!("{err:#}")).title("Could not change the jj config"),
                cx,
            );
        }
    }

    let mut menu = menu
        .label("Appearance")
        .menu_with_check(
            "Light",
            theme == ThemeChoice::Light,
            Box::new(actions::ThemeLight),
        )
        .menu_with_check(
            "Dark",
            theme == ThemeChoice::Dark,
            Box::new(actions::ThemeDark),
        )
        .menu_with_check(
            "Follow system",
            theme == ThemeChoice::System,
            Box::new(actions::ThemeSystem),
        )
        .separator()
        .label("Diffs open as");
    for (layout, label) in [
        (DiffLayout::Unified, "Unified"),
        (DiffLayout::Split, "Split"),
    ] {
        menu = menu.item(
            PopupMenuItem::new(label)
                .checked(display.diff == layout)
                .on_click(move |_, _, cx| settings::update_display(cx, |d| d.diff = layout)),
        );
    }
    menu = menu
        .separator()
        .label("Snapshot when Randori comes to the front");
    for (value, label) in [
        (None, "Automatic (off in repositories over 100k commits)"),
        (Some(true), "Always"),
        (Some(false), "Never"),
    ] {
        menu = menu.item(
            PopupMenuItem::new(label)
                .checked(auto_snapshot == value)
                .on_click(move |_, window, cx| {
                    let result = settings::set_auto_snapshot(cx, value);
                    report(result, window, cx)
                }),
        );
    }
    menu = menu
        .separator()
        .label("Bookmarks")
        .item(
            PopupMenuItem::new("Mark bookmarks that are not pushed")
                .checked(mark_unpushed)
                .on_click(move |_, window, cx| {
                    let result = settings::set_mark_unpushed(cx, !mark_unpushed);
                    report(result, window, cx)
                }),
        )
        .separator()
        .label("GitHub")
        .item(
            PopupMenuItem::new("Pull requests and avatars")
                .checked(display.github)
                .on_click(|_, _, cx| settings::update_display(cx, |d| d.github = !d.github)),
        );
    for (secs, label) in PULL_REFRESH_CHOICES {
        menu = menu.item(
            PopupMenuItem::new(label)
                .checked(display.pull_refresh_secs == secs)
                .disabled(!display.github)
                .on_click(move |_, _, cx| {
                    settings::update_display(cx, |d| d.pull_refresh_secs = secs)
                }),
        );
    }
    menu
}

/// The nearest ancestor that holds a `.jj` directory.
fn workspace_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| dir.join(".jj").is_dir())
        .map(Path::to_path_buf)
}

/// The selected tab's index among the tabs that are saved (failed tabs are
/// not); a failed selection falls back to the saved tab before it.
fn saved_active(failed: &[bool], active: usize) -> usize {
    let kept_before = failed.iter().take(active).filter(|f| !**f).count();
    let active_kept = failed.get(active).is_some_and(|f| !*f);
    if active_kept {
        kept_before
    } else {
        kept_before.saturating_sub(1)
    }
}

/// Splits saved tab paths into directories that still exist and ones that do not.
fn split_existing(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>) {
    paths.into_iter().partition(|path| path.is_dir())
}

fn tab_title(path: &Path) -> SharedString {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
        .into()
}

fn session_path() -> Option<PathBuf> {
    use etcetera::BaseStrategy as _;
    let strategy = etcetera::choose_base_strategy().ok()?;
    Some(strategy.config_dir().join("randori").join("session.json"))
}

fn load_session() -> Session {
    session_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn store_session(session: &Session) -> Result<()> {
    let path = session_path().context("no config directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(session)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_skips_workspaces_that_are_gone() {
        let here = std::env::temp_dir();
        let gone = here.join("randori-no-such-workspace-7f3a");
        let (existing, missing) = split_existing(vec![here.clone(), gone.clone()]);
        assert_eq!(existing, vec![here]);
        assert_eq!(missing, vec![gone]);
    }

    #[test]
    fn saved_active_counts_only_saved_tabs() {
        // [ok, failed] with the failed one selected: one tab saved, select it.
        assert_eq!(saved_active(&[false, true], 1), 0);
        // [failed, ok] with the ok one selected: it becomes the first.
        assert_eq!(saved_active(&[true, false], 1), 0);
        // [ok, failed, ok] selecting the last: second saved tab.
        assert_eq!(saved_active(&[false, true, false], 2), 1);
        // nothing failed: unchanged.
        assert_eq!(saved_active(&[false, false, false], 2), 2);
        // everything failed: nothing to select, stays in range of an empty list.
        assert_eq!(saved_active(&[true, true], 1), 0);
    }
}
