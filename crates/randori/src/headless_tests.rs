//! Flows of the app driven in a test window, with no screen: the tab
//! (`RepoView`) on a repository the jj CLI makes, its real key bindings and
//! actions, and jj's record read back with the CLI. They need `jj` on `PATH`:
//! without it they are skipped, and on CI (`CI` set) they fail. What only the
//! screen shows (a cut-off label, panels over each other) is the screenshot
//! harness's (`--screenshot`).

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use gpui::{Entity, TestAppContext, VisualTestContext};

use crate::repo_view::RepoView;
use crate::worker::Worker;

/// `jj` in `repo`, its output; jj's own config only (an empty user config
/// beside the repository, outside it), as the user's could change what it
/// makes.
fn jj(repo: &Path, args: &[&str]) -> String {
    let no_config = repo.with_file_name("no-user-config.toml");
    if !no_config.exists() {
        std::fs::write(&no_config, "").unwrap();
    }
    let out = Command::new("jj")
        .args(args)
        .current_dir(repo)
        .env("JJ_USER", "Randori Test")
        .env("JJ_EMAIL", "test@example.com")
        .env("JJ_CONFIG", &no_config)
        .output()
        .expect("jj runs");
    assert!(
        out.status.success(),
        "jj {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Whether `jj` is on `PATH`. Without it a test is skipped, except on CI
/// (`CI` set), where a skip would pass for a run.
fn has_jj() -> bool {
    let found = Command::new("jj").arg("--version").output().is_ok();
    assert!(
        found || std::env::var_os("CI").is_none(),
        "no jj on PATH: CI installs it before the tests"
    );
    found
}

/// A repository in a directory of its own, `repo` under `holder` (the empty
/// user config sits beside it). No repository config is set: jj keeps it in
/// the user's config directory (`~/.config/jj/repos`), which a test must not
/// write to.
fn init(holder: &Path) -> std::path::PathBuf {
    let repo = holder.join("repo");
    std::fs::create_dir(&repo).unwrap();
    jj(&repo, &["git", "init"]);
    repo
}

/// The tab on `repo` in a test window, loaded.
fn open<'a>(
    repo: &Path,
    cx: &'a mut TestAppContext,
) -> (Entity<RepoView>, &'a mut VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::install(cx);
        cx.set_global(crate::settings::Settings::load(Default::default()));
        crate::repo_view::bind_keys(cx);
    });
    let path = repo.to_path_buf();
    let (settings, _, _, _) =
        gg_lib::read_config(Some(&path)).expect("the repository's config is read");
    let (worker, events) = Worker::spawn(Some(path.clone()), settings).expect("a worker");
    // every revision, as All branches: the rows do not lean on the user's
    // `revsets.log`, which the app reads
    let query = Some(crate::repo_view::ALL_BRANCHES_QUERY.to_owned());
    let (view, cx) = cx
        .add_window_view(|window, cx| RepoView::new(worker, events, Some(path), query, window, cx));
    settle(&view, cx);
    // as the app does once a tab has loaded: the keys go to its list
    view.update_in(cx, |view, window, cx| view.focus_log(window, cx));
    settle(&view, cx);
    (view, cx)
}

/// Waits, in real time, until the tab has nothing on its way: its worker is
/// a thread of its own, which the test's clock does not drive.
fn settle(view: &Entity<RepoView>, cx: &mut VisualTestContext) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        if view.read_with(cx, |view, _| view.is_settled()) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the tab did not settle"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Keys pressed through the window's real bindings, then a wait for what
/// they started.
fn keys(view: &Entity<RepoView>, keystrokes: &str, cx: &mut VisualTestContext) {
    for key in keystrokes.split(' ') {
        cx.simulate_keystrokes(key);
        settle(view, cx);
    }
}

fn changed(repo: &Path, rev: &str) -> String {
    jj(
        repo,
        &["--ignore-working-copy", "diff", "-r", rev, "--name-only"],
    )
    .lines()
    .collect::<Vec<_>>()
    .join(" ")
}

/// A described base with a.txt, and a working copy changing it and adding c.txt.
fn commit_repo(holder: &Path) -> std::path::PathBuf {
    let dir = &init(holder);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    jj(dir, &["describe", "-m", "base"]);
    jj(dir, &["new"]);
    std::fs::write(dir.join("a.txt"), "a\nb\n").unwrap();
    std::fs::write(dir.join("c.txt"), "c\n").unwrap();
    jj(dir, &["status"]);
    dir.clone()
}

/// D, a summary typed, ⌘↩ (Ctrl+Enter off macOS: `secondary`) with nothing
/// included: `jj commit` of every change, the new working copy empty.
#[gpui::test]
fn commit_every_change(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = commit_repo(dir.path());
    let (view, cx) = open(&repo, cx);
    // the working copy's row marks a.txt modified and c.txt added
    let counts = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| view.wip_counts())
            .map(|c| (c.modified, c.added, c.deleted))
    };
    assert_eq!(counts(&view, cx), Some((1, 1, 0)));

    keys(&view, "d f i x", cx);
    // typed in the inspector's commit box, the summary is in the row too
    assert_eq!(
        view.read_with(cx, |view, cx| view.summaries(cx)),
        ("fix".into(), "fix".into())
    );
    keys(&view, "secondary-enter", cx);

    // the new working copy changes nothing: its row has no marks
    assert_eq!(counts(&view, cx), Some((0, 0, 0)));

    let repo = repo.as_path();
    assert_eq!(
        jj(
            repo,
            &[
                "--ignore-working-copy",
                "log",
                "-r",
                "@-",
                "--no-graph",
                "-T",
                "description"
            ]
        ),
        "fix\n"
    );
    assert_eq!(changed(repo, "@-"), "a.txt c.txt");
    assert_eq!(changed(repo, "@"), "");
}

/// The summary typed in the working copy's row of the log, as GitKraken's
/// WIP row takes it: letters that are the list's keys (b, e, n, d) are typed,
/// the commit box has the same summary, and ⌘↩ there commits every change.
#[gpui::test]
fn commit_from_the_working_copy_s_row(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = commit_repo(dir.path());
    let (view, cx) = open(&repo, cx);
    view.update_in(cx, |view, window, cx| view.focus_row_summary(window, cx));
    settle(&view, cx);

    let summaries = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, cx| view.summaries(cx))
    };
    // Escape goes back to the list, whose D then focuses the commit box
    keys(&view, "b e n escape d d", cx);
    assert_eq!(summaries(&view, cx), ("bend".into(), "bend".into()));

    keys(&view, "secondary-enter", cx);

    // the box and the row are cleared for the next commit
    assert_eq!(summaries(&view, cx), (String::new(), String::new()));
    let repo = repo.as_path();
    assert_eq!(
        jj(
            repo,
            &[
                "--ignore-working-copy",
                "log",
                "-r",
                "@-",
                "--no-graph",
                "-T",
                "description"
            ]
        ),
        "bend\n"
    );
    assert_eq!(changed(repo, "@-"), "a.txt c.txt");
    assert_eq!(changed(repo, "@"), "");
}

/// base; qa1 (qa) on it; w1, w2 (topic) on base with the working copy on
/// topic.
fn branch_repo(holder: &Path) -> std::path::PathBuf {
    let dir = &init(holder);
    for (file, message) in [("base.txt", "base"), ("qa.txt", "qa1")] {
        std::fs::write(dir.join(file), "x\n").unwrap();
        jj(dir, &["commit", "-m", message]);
    }
    jj(dir, &["bookmark", "create", "-r", "@-", "qa"]);
    jj(dir, &["new", "@--"]);
    for (file, message) in [("w1.txt", "w1"), ("w2.txt", "w2")] {
        std::fs::write(dir.join(file), "x\n").unwrap();
        jj(dir, &["commit", "-m", message]);
    }
    jj(dir, &["bookmark", "create", "-r", "@-", "topic"]);
    dir.clone()
}

/// w1 selected, R, qa1 chosen with the keys and Enter: as `jj rebase -r w1
/// -o qa`, w1 alone moves onto qa1 and w2 onto base.
#[gpui::test]
fn rebase_a_revision_from_the_keyboard(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = branch_repo(dir.path());
    let (view, cx) = open(&repo, cx);

    // the log from the working copy: @, w2, w1, qa1, base
    keys(&view, "down down r down enter", cx);

    let repo = repo.as_path();
    let log = |rev: &str| {
        jj(
            repo,
            &[
                "--ignore-working-copy",
                "log",
                "-r",
                rev,
                "--no-graph",
                "-T",
                "description",
            ],
        )
    };
    assert_eq!(log("description(exact:\"w1\n\")-"), "qa1\n");
    assert_eq!(log("description(exact:\"w2\n\")-"), "base\n");
}
