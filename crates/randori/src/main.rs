//! Randori: a desktop client for Jujutsu built with gpui-kit.
//!
//! The jj work is done by GG's worker (`gg_lib::worker`); this crate owns only
//! the windows and views.

// gg_lib marks itself deprecated as a public library. Randori vendors it in the
// same workspace on purpose, so the warning does not apply here.
#![allow(deprecated)]

mod graph;
mod repo_view;
mod worker;

use std::path::PathBuf;

use anyhow::Result;
use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::{AppContext as _, WindowBounds, WindowOptions, px, size};

use repo_view::RepoView;
use worker::Worker;

const USAGE: &str = "Usage: randori [options] [workspace]
Opens the jj workspace at the given path (default: current directory).

Options:
  --revset <query>     start with this revset instead of the workspace default
  --light              use the light theme
  --check              load the workspace through the worker, print a summary and exit
  --select-row <n>     select the n-th log row once loaded
  --screenshot <png>   render the first screen to a PNG and exit (needs --features screenshot)
  --keys <k1,k2,...>   with --screenshot: press these keys first, e.g. \"down,j,end\"";

#[derive(Default)]
struct Args {
    workspace: Option<PathBuf>,
    revset: Option<String>,
    light: bool,
    check: bool,
    select_row: Option<usize>,
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
            _ if args.workspace.is_none() => args.workspace = Some(PathBuf::from(arg)),
            _ => fail(),
        }
    }
    args
}

fn main() -> Result<()> {
    let args = parse_args();

    // Same resolution as GG: an explicit path, or the current directory.
    let repo_path = gg_lib::config::resolve_repo_path(args.workspace.as_deref());
    let (settings, _, _, _) = gg_lib::read_config(repo_path.as_deref())?;
    let (worker, events) = Worker::spawn(args.workspace.clone(), settings)?;

    if args.check {
        return run_check(&worker, args.workspace, args.revset);
    }

    #[cfg(not(feature = "screenshot"))]
    if args.screenshot.is_some() {
        anyhow::bail!("--screenshot needs a build with `--features screenshot`");
    }

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            repo_view::bind_keys(cx);
            Theme::change(
                if args.light {
                    ThemeMode::Light
                } else {
                    ThemeMode::Dark
                },
                None,
                cx,
            );

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1440.), px(900.)), cx)),
                window_min_size: Some(size(px(960.), px(600.))),
                app_id: Some("io.github.jeonsu0609.randori".into()),
                ..TitleBar::window_options()
            };
            let workspace = args.workspace.clone();
            let revset = args.revset.clone();
            let (window, view) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| RepoView::new(worker, events, workspace, revset, window, cx))
            })
            .expect("failed to open the main window");
            cx.activate(true);

            let select_row = args.select_row;
            let screenshot = args.screenshot.clone();
            let keys = args.keys.clone();
            cx.spawn(async move |cx| {
                // Wait for the workspace to load, then apply one-shot startup options.
                wait_until(cx, &view, |v| v.is_settled()).await;
                if let Some(row) = select_row {
                    view.update(cx, |v, cx| v.select_row(row, cx));
                    wait_until(cx, &view, |v| v.is_settled()).await;
                }
                #[cfg(feature = "screenshot")]
                if let Some(path) = screenshot {
                    // Drive the real key bindings, as a user would, before capturing.
                    for key in keys {
                        let Ok(keystroke) =
                            gpui_kit::Keystroke::parse(Box::leak(key.into_boxed_str()))
                        else {
                            eprintln!("keys: cannot parse a key");
                            continue;
                        };
                        let _ = window.update(cx, |_, window, cx| {
                            window.dispatch_keystroke(keystroke, cx);
                        });
                        wait_until(cx, &view, |v| v.is_settled()).await;
                    }
                    let selected = view.read_with(cx, |v, _| v.selected_row());
                    println!("selected row: {selected:?}");
                    // Let layout and the diff settle for a few frames before capturing.
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(800))
                        .await;
                    let _ = window.update(cx, |_, window, cx| {
                        match window.render_to_image() {
                            Ok(image) => match image.save(&path) {
                                Ok(()) => println!("screenshot: {}", path.display()),
                                Err(err) => eprintln!("screenshot: {err}"),
                            },
                            Err(err) => eprintln!("screenshot: {err:#}"),
                        }
                        cx.quit();
                    });
                }
                #[cfg(not(feature = "screenshot"))]
                let _ = (screenshot, window, keys);
            })
            .detach();
        });
    Ok(())
}

async fn wait_until(
    cx: &mut gpui_kit::AsyncApp,
    view: &gpui_kit::Entity<RepoView>,
    ready: impl Fn(&RepoView) -> bool,
) {
    loop {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(100))
            .await;
        if view.read_with(cx, |v, _| ready(v)) {
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
    let page = worker.query_log(query.clone())?;
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
