//! Randori: a desktop client for Jujutsu built with gpui-kit.
//!
//! The jj work is done by GG's worker (`gg_lib::worker`); this crate owns only
//! the windows and views.

mod app_view;
mod assets;
mod github;
mod graph;
#[cfg(feature = "screenshot")]
mod harness;
mod harness_mark;
mod issue_links;
mod repo_view;
mod settings;
mod settings_window;
mod terminal;
mod theme;
mod worker;

use std::path::PathBuf;

use anyhow::Result;
use gpui_kit::component::TitleBar;
use gpui_kit::{AppContext as _, WindowBounds, WindowOptions, px, size};

use app_view::RandoriApp;
use repo_view::RepoView;
use worker::Worker;

const USAGE: &str = "Usage: randori [options] [workspace...]
Opens each jj workspace in a tab, after the tabs of the previous session.
With no workspace and no saved tabs, opens the current directory.

Options:
  --revset <query>     start with this revset instead of the workspace default
  --light              use the light theme
  --check              load the workspace through the worker, print a summary and exit
  --select-row <n>     select the n-th log row once loaded
  --size <WxH>         open the window this size in points, e.g. 1000x700
  --screenshot <png>   render the first screen to a PNG and exit (needs --features screenshot)
  --keys <k1,k2,...>   with --screenshot: press these keys first, e.g. \"down,j,end\";
                       action:<name> runs an action as a menu would, e.g. action:randori::SquashFile";

#[derive(Default)]
struct Args {
    workspaces: Vec<PathBuf>,
    revset: Option<String>,
    light: bool,
    check: bool,
    select_row: Option<usize>,
    /// The window's size in points, for screenshots of a narrow window.
    size: Option<(f32, f32)>,
    screenshot: Option<PathBuf>,
    keys: Vec<String>,
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let mut it = std::env::args_os().skip(1);
    let fail = || -> ! {
        eprintln!("{USAGE}");
        std::process::exit(2)
    };
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            Some("--check") => args.check = true,
            Some("--light") => args.light = true,
            Some("--revset") => {
                args.revset = Some(
                    it.next()
                        .and_then(|v| v.into_string().ok())
                        .unwrap_or_else(|| fail()),
                )
            }
            Some("--select-row") => {
                args.select_row = Some(
                    it.next()
                        .and_then(|v| v.to_str()?.parse().ok())
                        .unwrap_or_else(|| fail()),
                )
            }
            Some("--size") => {
                args.size = Some(
                    it.next()
                        .and_then(|v| {
                            let (w, h) = v.to_str()?.split_once('x')?;
                            Some((w.parse().ok()?, h.parse().ok()?))
                        })
                        .unwrap_or_else(|| fail()),
                )
            }
            Some("--screenshot") => {
                args.screenshot = Some(it.next().map(PathBuf::from).unwrap_or_else(|| fail()))
            }
            Some("--keys") => {
                args.keys = it
                    .next()
                    .and_then(|v| v.into_string().ok())
                    .unwrap_or_else(|| fail())
                    .split(',')
                    .map(|key| key.trim().to_owned())
                    .filter(|key| !key.is_empty())
                    .collect()
            }
            Some(flag) if flag.starts_with('-') => fail(),
            _ => args.workspaces.push(PathBuf::from(arg)),
        }
    }
    args
}

fn main() -> Result<()> {
    // git and ssh run this binary as their askpass helper (GG_ASKPASS_SOCKET is
    // set); it relays the prompt to the window that started the operation.
    if let Some(result) = gg_lib::askpass::run_askpass() {
        return result;
    }

    let args = parse_args();

    if args.check {
        // Same resolution as GG: an explicit path, or the current directory.
        let workspace = args.workspaces.first().cloned();
        let repo_path = gg_lib::config::resolve_repo_path(workspace.as_deref());
        let (settings, _, _, _) = gg_lib::read_config(repo_path.as_deref())?;
        let (worker, _events) = Worker::spawn(workspace.clone(), settings)?;
        return run_check(&worker, workspace, args.revset);
    }

    #[cfg(not(feature = "screenshot"))]
    if args.screenshot.is_some() {
        anyhow::bail!("--screenshot needs a build with `--features screenshot`");
    }

    gpui_kit::application()
        .with_assets(assets::RandoriAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            theme::install(cx);
            // GitHub avatars (`img(url)`) and the pull request queries. Its
            // read timeout needs a Tokio runtime where the body is read, which
            // GPUI's executors are not; `repo_view::github_access::within` times
            // GitHub's requests out instead.
            match reqwest_client::ReqwestClient::user_agent(concat!(
                "Randori/",
                env!("CARGO_PKG_VERSION")
            )) {
                Ok(client) => cx.set_http_client(std::sync::Arc::new(client)),
                Err(err) => log::warn!("no HTTP client, GitHub features are off: {err:#}"),
            }
            repo_view::bind_keys(cx);
            app_view::bind_keys(cx);
            // after the tab's and the app's, so the terminal's keys win
            terminal::bind_keys(cx);
            settings_window::find_installed_fonts(cx);

            let window_size = args.size.unwrap_or((1440., 900.));
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(
                    size(px(window_size.0), px(window_size.1)),
                    cx,
                )),
                window_min_size: Some(size(px(960.), px(600.))),
                app_id: Some("io.github.jeonsu0609.randori".into()),
                ..TitleBar::window_options()
            };
            let workspaces = args.workspaces.clone();
            let revset = args.revset.clone();
            // Screenshots must not replace the user's saved tabs with sample workspaces.
            let persist = args.screenshot.is_none();
            let args_light = args.light;
            let (window, app) = gpui_kit::open_window(options, cx, |window, cx| {
                // --light overrides the saved appearance for this run only.
                let theme = args_light.then_some(app_view::ThemeChoice::Light);
                cx.new(|cx| RandoriApp::new(workspaces, revset, persist, theme, window, cx))
            })
            .expect("failed to open the main window");
            cx.activate(true);
            install_app_menu(cx);
            cx.set_global(settings_window::MainApp {
                app: app.downgrade(),
                display: app.read(cx).on_display(),
            });
            // Randori has one main window; closing it ends the app instead of
            // leaving it running with nothing to show (or only Settings).
            cx.on_window_closed(|cx, _| {
                let windows = cx.windows();
                if windows
                    .iter()
                    .all(|window| settings_window::is_settings_window(*window, cx))
                {
                    cx.quit();
                }
            })
            .detach();
            cx.on_action(|_: &app_view::actions::OpenSettings, cx| settings_window::open(cx));
            // The window handles Quit itself (it asks about unsaved
            // descriptions); this runs only when no window took it.
            cx.on_action(|_: &app_view::actions::Quit, cx| cx.quit());
            cx.on_action(|_: &app_view::actions::Hide, cx| cx.hide());
            cx.on_action(|_: &app_view::actions::HideOthers, cx| cx.hide_other_apps());
            cx.on_action(|_: &app_view::actions::ShowAll, cx| cx.unhide_other_apps());

            let select_row = args.select_row;
            let screenshot = args.screenshot.clone();
            let keys = args.keys.clone();
            cx.spawn(async move |cx| {
                // Wait for the workspace to load, then apply one-shot startup options.
                wait_until_settled(cx, &app).await;
                if let Some(row) = select_row
                    && let Some(view) = app.read_with(cx, |app, _| app.active_view())
                {
                    view.update(cx, |v, cx| v.select_row(row, cx));
                    wait_until_settled(cx, &app).await;
                }
                #[cfg(feature = "screenshot")]
                if let Some(path) = screenshot {
                    harness::run(cx, &app, window, keys, path).await;
                }
                #[cfg(not(feature = "screenshot"))]
                let _ = (screenshot, window, keys);
            })
            .detach();
        });
    Ok(())
}

/// Waits until the selected tab has loaded (or there is no tab).
pub(crate) async fn wait_until_settled(
    cx: &mut gpui_kit::AsyncApp,
    app: &gpui_kit::Entity<RandoriApp>,
) {
    loop {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(100))
            .await;
        let settled = match app.read_with(cx, |app, _| app.active_view()) {
            Some(view) => view.read_with(cx, |v: &RepoView, _| v.is_settled()),
            None => true,
        };
        if settled {
            return;
        }
    }
}

/// Headless smoke test of the worker bridge: no window is opened.
fn run_check(worker: &Worker, workspace: Option<PathBuf>, revset: Option<String>) -> Result<()> {
    use gg_lib::messages::RepoConfig;

    let RepoConfig::Workspace {
        absolute_path,
        latest_query,
        status,
        ..
    } = worker.open_workspace(workspace)?
    else {
        anyhow::bail!("the workspace did not load");
    };
    let query = revset.unwrap_or(latest_query);
    let page = worker.query_log(query.clone(), false)?;
    println!("workspace: {}", absolute_path.0);
    println!("query:     {query}");
    println!("operation: {}", status.operation_description);
    println!(
        "log page:  {} rows{}",
        page.rows.len(),
        if page.has_more {
            " (more available)"
        } else {
            ""
        }
    );
    for row in page.rows.iter().take(5) {
        let rev = &row.revision;
        println!(
            "  {:>2} {} {:<12} {}",
            row.location.0,
            if rev.is_working_copy { "@" } else { " " },
            rev.id.change.prefix.clone() + &rev.id.change.rest[..6.min(rev.id.change.rest.len())],
            rev.description
                .lines
                .first()
                .map(String::as_str)
                .unwrap_or("")
        );
    }
    Ok(())
}

/// The menu bar (macOS) or application menu. Shortcuts shown next to items
/// come from the key bindings.
fn install_app_menu(cx: &mut gpui_kit::App) {
    use app_view::actions as app;
    use gpui_kit::{Menu, MenuItem, SystemMenuType};
    use repo_view::actions as repo;
    cx.set_menus([
        Menu::new("Randori").items([
            MenuItem::action("Settings…", app::OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Randori", app::Hide),
            MenuItem::action("Hide Others", app::HideOthers),
            MenuItem::action("Show All", app::ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Randori", app::Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("Open Workspace…", app::OpenRepository),
            MenuItem::separator(),
            MenuItem::action("Close Tab", app::CloseTab),
        ]),
        Menu::new("View").items([
            MenuItem::action("Command Palette", app::OpenCommandPalette),
            MenuItem::separator(),
            MenuItem::action("My Work", repo::ShowMyWork),
            MenuItem::action("All Branches", repo::ShowAllBranches),
            MenuItem::separator(),
            MenuItem::action("By Date", repo::OrderByDate),
            MenuItem::action("Grouped by Branch", repo::GroupByBranch),
            MenuItem::action("Show or Hide Side Panel", repo::ToggleSidebar),
            MenuItem::action("Show or Hide Terminal", repo::ToggleTerminal),
            MenuItem::separator(),
            MenuItem::action("Widen Original Side", repo::WidenOriginalSide),
            MenuItem::action("Widen Modified Side", repo::WidenModifiedSide),
            MenuItem::action("Even Split Sides", repo::EvenSplitSides),
            MenuItem::separator(),
            MenuItem::action("Reload Revisions", repo::Reload),
            MenuItem::action("Refresh Pull Requests", repo::RefreshPullRequests),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Next Tab", app::NextTab),
            MenuItem::action("Previous Tab", app::PreviousTab),
        ]),
    ]);
}
