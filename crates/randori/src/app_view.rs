//! The window: a tab per jj workspace, like Fork or GitKraken.
//!
//! Each tab owns a [`RepoView`] and, through it, its own jj worker thread
//! (jj-lib is not thread-safe, so workspaces never share one). The open tabs
//! are saved and restored on the next launch.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _,
    TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    notification::Notification,
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::{
    AppContext as _, Context, Entity, Image, ImageFormat, InteractiveElement as _, IntoElement,
    KeyBinding, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Window,
    div, img, prelude::FluentBuilder as _,
};
use serde::{Deserialize, Serialize};

use crate::repo_view::RepoView;
use crate::worker::Worker;

const LOGO: &[u8] = include_bytes!("../assets/icon/randori-128.png");
const APP_CONTEXT: &str = "RandoriApp";

pub mod actions {
    gpui_kit::actions!(randori, [OpenRepository, CloseTab, NextTab, PreviousTab]);
}

/// Binds the tab keys. Call once at startup.
pub fn bind_keys(cx: &mut gpui_kit::App) {
    use actions::*;
    // `cmd-` on macOS, `ctrl-` elsewhere.
    let (open, close) = if cfg!(target_os = "macos") {
        ("cmd-o", "cmd-w")
    } else {
        ("ctrl-o", "ctrl-w")
    };
    cx.bind_keys([
        KeyBinding::new(open, OpenRepository, Some(APP_CONTEXT)),
        KeyBinding::new(close, CloseTab, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-tab", NextTab, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some(APP_CONTEXT)),
    ]);
}

struct RepoTab {
    path: PathBuf,
    title: SharedString,
    view: Entity<RepoView>,
}

/// Tabs restored on the next launch.
#[derive(Serialize, Deserialize, Default)]
struct Session {
    tabs: Vec<PathBuf>,
    active: usize,
}

pub struct RandoriApp {
    tabs: Vec<RepoTab>,
    active: usize,
    logo: Arc<Image>,
    /// Whether open tabs are saved and restored (off for screenshots).
    persist: bool,
}

impl RandoriApp {
    /// Restores the previous session (when `persist`), then opens `paths` and
    /// selects the first of them. With nothing to open, falls back to the
    /// current directory if it is a jj workspace.
    pub fn new(
        paths: Vec<PathBuf>,
        revset: Option<String>,
        persist: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut app = Self {
            tabs: Vec::new(),
            active: 0,
            logo: Arc::new(Image::from_bytes(ImageFormat::Png, LOGO.to_vec())),
            persist,
        };

        let session = if persist {
            load_session()
        } else {
            Session::default()
        };
        for path in &session.tabs {
            if let Err(err) = app.open_tab(path.clone(), None, window, cx) {
                log::warn!("cannot restore {}: {err:#}", path.display());
            }
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
        if let Some(ix) = self.tabs.iter().position(|tab| tab.path == path) {
            self.activate(ix, window, cx);
            return Ok(ix);
        }

        let repo_path = gg_lib::config::resolve_repo_path(Some(&path));
        let (settings, _, _, _) =
            gg_lib::read_config(repo_path.as_deref()).context("reading jj config")?;
        let (worker, events) = Worker::spawn(Some(path.clone()), settings)?;
        let view =
            cx.new(|cx| RepoView::new(worker, events, Some(path.clone()), revset, window, cx));
        self.tabs.push(RepoTab {
            title: tab_title(&path),
            path,
            view,
        });
        let ix = self.tabs.len() - 1;
        self.activate(ix, window, cx);
        Ok(ix)
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = ix;
        let view = self.tabs[ix].view.clone();
        view.update(cx, |view, cx| view.focus_log(window, cx));
        self.save_session();
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        // Dropping the view drops its worker handle; the worker thread then ends.
        self.tabs.remove(ix);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if ix < self.active {
            self.active -= 1;
        }
        if !self.tabs.is_empty() {
            self.activate(self.active, window, cx);
        }
        self.save_session();
        cx.notify();
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

    fn save_session(&self) {
        if !self.persist {
            return;
        }
        let session = Session {
            tabs: self.tabs.iter().map(|tab| tab.path.clone()).collect(),
            active: self.active,
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
                                        app.close_tab(ix, window, cx);
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
            .child(div().flex_1())
            .child(
                Button::new("rail-settings")
                    .ghost()
                    .icon(IconName::Settings)
                    .tooltip("Settings")
                    .disabled(true),
            )
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
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|app, _: &actions::OpenRepository, window, cx| {
                app.prompt_open(window, cx)
            }))
            .on_action(cx.listener(|app, _: &actions::CloseTab, window, cx| {
                app.close_tab(app.active, window, cx)
            }))
            .on_action(
                cx.listener(|app, _: &actions::NextTab, window, cx| app.cycle(1, window, cx)),
            )
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
