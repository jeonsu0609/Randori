//! Flows of the app driven in a test window, with no screen: the tab
//! (`RepoView`) on a repository the jj CLI makes, its real key bindings and
//! actions, and jj's record read back with the CLI. They need `jj` on `PATH`:
//! without it they are skipped, and on CI (`CI` set) they fail. What only the
//! screen shows (a cut-off label, panels over each other) is the screenshot
//! harness's (`--screenshot`).

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

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
    // in the root the app's windows have (`gpui_kit::open_window`), which
    // holds the notifications and dialogs
    let built = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot = built.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| RepoView::new(worker, events, Some(path), query, window, cx));
        *slot.borrow_mut() = Some(view.clone());
        gpui_kit::base::Root::new(view, window, cx)
    });
    let view = built.borrow_mut().take().expect("the tab is built");
    settle(&view, cx);
    // as the app does once a tab has loaded: the keys go to its list
    view.update_in(cx, |view, window, cx| view.focus_log(window, cx));
    settle(&view, cx);
    (view, cx)
}

/// A second tab on `repo` beside the first, as another tab of the window,
/// loaded.
fn open_beside(repo: &Path, cx: &mut VisualTestContext) -> Entity<RepoView> {
    let path = repo.to_path_buf();
    let (settings, _, _, _) =
        gg_lib::read_config(Some(&path)).expect("the repository's config is read");
    let (worker, events) = Worker::spawn(Some(path.clone()), settings).expect("a worker");
    let query = Some(crate::repo_view::ALL_BRANCHES_QUERY.to_owned());
    let view = cx.update(|window, cx| {
        cx.new(|cx| RepoView::new(worker, events, Some(path), query, window, cx))
    });
    settle(&view, cx);
    view
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

/// A changed file clicked, ↓ ↑ and End go through the inspector's files as
/// they do through the revisions: the diff follows the file and the
/// revision stays selected. Escape gives the keys back to the revisions.
#[gpui::test]
fn arrow_keys_go_through_the_changed_files(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = commit_repo(dir.path());
    let (view, cx) = open(&repo, cx);
    let state = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| (view.selected_row(), view.shown_file()))
    };
    let row = view.read_with(cx, |view, _| view.selected_row());
    assert_eq!(state(&view, cx), (row, Some("a.txt".into())));

    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));
    keys(&view, "down", cx);
    assert_eq!(state(&view, cx), (row, Some("c.txt".into())));
    // the last file stays the last
    keys(&view, "down", cx);
    assert_eq!(state(&view, cx), (row, Some("c.txt".into())));
    keys(&view, "up", cx);
    assert_eq!(state(&view, cx), (row, Some("a.txt".into())));
    keys(&view, "end", cx);
    assert_eq!(state(&view, cx), (row, Some("c.txt".into())));

    keys(&view, "escape down", cx);
    assert_ne!(view.read_with(cx, |view, _| view.selected_row()), row);
}

/// Enter in the changed files opens the file's full view, as a double click
/// does, and Escape there closes it, back to the files.
#[gpui::test]
fn enter_opens_the_file_s_full_view(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = commit_repo(dir.path());
    let (view, cx) = open(&repo, cx);
    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));

    keys(&view, "down enter", cx);
    let open = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| (view.full_view_open(), view.shown_file()))
    };
    assert_eq!(open(&view, cx), (true, Some("c.txt".into())));
    // closed, the keys go back to the files
    keys(&view, "escape up", cx);
    assert_eq!(open(&view, cx), (false, Some("a.txt".into())));
}

/// With a file's full view open and a file clicked, ↓ goes on through the
/// files (the full view following), and Escape closes a step at a time: the
/// full view first, the keys staying with the files, then the files.
#[gpui::test]
fn escape_closes_the_full_view_before_leaving_the_files(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = commit_repo(dir.path());
    let (view, cx) = open(&repo, cx);
    let row = view.read_with(cx, |view, _| view.selected_row());
    keys(&view, "secondary-shift-d", cx);
    assert!(view.read_with(cx, |view, _| view.full_view_open()));

    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));
    keys(&view, "down", cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.shown_file()),
        Some("c.txt".into())
    );
    keys(&view, "escape", cx);
    assert!(!view.read_with(cx, |view, _| view.full_view_open()));
    // the keys are still the files'
    keys(&view, "up", cx);
    assert_eq!(
        view.read_with(cx, |view, _| (view.selected_row(), view.shown_file())),
        (row, Some("a.txt".into()))
    );
    keys(&view, "escape down", cx);
    assert_ne!(view.read_with(cx, |view, _| view.selected_row()), row);
}

/// Enter and Escape go in and out a step at a time: the revisions, their
/// changed files, a file's full view, and back.
#[gpui::test]
fn enter_and_escape_go_in_and_out(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = commit_repo(dir.path());
    let (view, cx) = open(&repo, cx);
    let state = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| {
            (
                view.selected_row(),
                view.shown_file(),
                view.full_view_open(),
            )
        })
    };
    let row = view.read_with(cx, |view, _| view.selected_row());

    // the revisions: Enter, and ↓ goes through the files
    keys(&view, "enter down", cx);
    assert_eq!(state(&view, cx), (row, Some("c.txt".into()), false));
    // the files: Enter opens the full view, Escape closes it
    keys(&view, "enter", cx);
    assert_eq!(state(&view, cx), (row, Some("c.txt".into()), true));
    keys(&view, "escape up", cx);
    assert_eq!(state(&view, cx), (row, Some("a.txt".into()), false));
    // the files: Escape, and ↓ goes through the revisions again
    keys(&view, "escape down", cx);
    assert_ne!(view.read_with(cx, |view, _| view.selected_row()), row);
}

/// The changed file's menu copies its whole path, in the system's own form.
#[gpui::test]
fn the_file_s_path_is_copied(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = commit_repo(dir.path());
    let (view, cx) = open(&repo, cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.shown_file()),
        Some("a.txt".into())
    );
    let copied = |cx: &mut VisualTestContext| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    };

    cx.dispatch_action(crate::repo_view::actions::CopyFullFilePath);
    assert_eq!(
        std::path::PathBuf::from(copied(cx)).canonicalize().unwrap(),
        repo.join("a.txt").canonicalize().unwrap()
    );
}

/// The menu's copies: the selected revision's ids, whole, as jj writes them.
#[gpui::test]
fn a_revision_s_ids_are_copied(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = commit_repo(dir.path());
    let (_view, cx) = open(&repo, cx);
    let copied = |cx: &mut VisualTestContext| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    };
    let id = |template: &str| {
        jj(
            &repo,
            &[
                "--ignore-working-copy",
                "log",
                "-r",
                "@",
                "--no-graph",
                "-T",
                template,
            ],
        )
    };

    cx.dispatch_action(crate::repo_view::actions::CopyChangeId);
    assert_eq!(copied(cx), id("change_id"));
    cx.dispatch_action(crate::repo_view::actions::CopyCommitId);
    assert_eq!(copied(cx), id("commit_id"));
}

/// The link to a commit is offered where GitHub has it: base, under
/// `qa@origin`, but not w2, on no remote bookmark.
#[gpui::test]
fn a_link_is_offered_for_a_commit_on_github(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = branch_repo(dir.path());
    jj(
        &repo,
        &[
            "git",
            "remote",
            "add",
            "origin",
            "https://github.com/owner/name.git",
        ],
    );
    // qa as if fetched from origin: its git ref, imported
    let qa = jj(&repo, &["log", "-r", "qa", "--no-graph", "-T", "commit_id"]);
    let git_dir = jj(&repo, &["git", "root"]);
    let out = Command::new("git")
        .args([
            "--git-dir",
            git_dir.trim(),
            "update-ref",
            "refs/remotes/origin/qa",
            &qa,
        ])
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    jj(&repo, &["git", "import"]);
    let base = jj(
        &repo,
        &["log", "-r", "qa-", "--no-graph", "-T", "commit_id"],
    );
    let (view, cx) = open(&repo, cx);

    // the log from the working copy: @, w2, w1, qa1, base
    let (base_link, w2) = view.read_with(cx, |view, _| (view.link_of_row(4), view.link_of_row(1)));
    assert_eq!(
        base_link,
        Some(format!("https://github.com/owner/name/commit/{base}"))
    );
    assert_eq!(w2, None);
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

/// w2 selected, Squash into parent: as `jj squash -r w2`, w1 has both files
/// and both descriptions, and w2 is gone.
#[gpui::test]
fn squash_a_revision_into_its_parent(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = branch_repo(dir.path());
    let (view, cx) = open(&repo, cx);

    // the log from the working copy: @, w2, w1, qa1, base
    keys(&view, "down", cx);
    cx.dispatch_action(crate::repo_view::actions::SquashRevision);
    settle(&view, cx);

    assert_eq!(
        changed(&repo, "description(substring:\"w1\")"),
        "w1.txt w2.txt"
    );
    let w2 = jj(
        &repo,
        &[
            "--ignore-working-copy",
            "log",
            "-r",
            "description(exact:\"w2\n\")",
            "--no-graph",
            "-T",
            "change_id",
        ],
    );
    assert_eq!(w2, "");
}

/// Squash into parent is not offered where jj would refuse it: on qa1, whose
/// parent (base) a tag makes immutable. w2's parent is not.
#[gpui::test]
fn no_squash_into_an_immutable_parent(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = branch_repo(dir.path());
    jj(&repo, &["tag", "set", "-r", "qa-", "v1"]);
    let (view, cx) = open(&repo, cx);

    // the log from the working copy: @, w2, w1, qa1, base
    let (qa1, w2) = view.read_with(cx, |view, _| {
        (view.squash_refused(3), view.squash_refused(1))
    });
    assert!(qa1);
    assert!(!w2);
}

/// In the inspector, a click on Commit copies the whole commit id and one on a
/// parent selects it: w2's is w1.
#[gpui::test]
fn the_inspector_s_commit_copies_and_its_parent_goes(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = branch_repo(dir.path());
    let (view, cx) = open(&repo, cx);

    // the log from the working copy: @, w2, w1, qa1, base
    keys(&view, "down", cx);
    view.update(cx, |view, cx| view.click_commit(cx));
    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    let w2 = jj(
        &repo,
        &[
            "--ignore-working-copy",
            "log",
            "-r",
            "description(exact:\"w2\n\")",
            "--no-graph",
            "-T",
            "commit_id",
        ],
    );
    assert_eq!(copied, w2);

    view.update_in(cx, |view, window, cx| view.click_parent(0, window, cx));
    settle(&view, cx);
    assert_eq!(view.read_with(cx, |view, _| view.selected_row()), Some(2));
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

/// w2 selected, ⌘-click on qa1: what changes from qa1's tree to w2's (qa.txt
/// gone, w1.txt and w2.txt added), not either's own change. Esc ends it, and
/// so does a plain click, the selection then where it was clicked.
#[gpui::test]
fn compare_two_revisions(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = branch_repo(dir.path());
    let (view, cx) = open(&repo, cx);

    // the log from the working copy: @, w2, w1, qa1, base
    keys(&view, "down", cx);
    let compared = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        settle(view, cx);
        view.read_with(cx, |view, _| view.compared_files())
    };
    view.update(cx, |view, cx| view.click_row(3, true, cx));
    assert_eq!(
        compared(&view, cx),
        Some(vec![
            "D qa.txt".to_owned(),
            "A w1.txt".to_owned(),
            "A w2.txt".to_owned(),
        ])
    );
    assert_eq!(view.read_with(cx, |view, _| view.selected_row()), Some(1));

    keys(&view, "escape", cx);
    assert_eq!(compared(&view, cx), None);
    assert_eq!(view.read_with(cx, |view, _| view.selected_row()), Some(1));

    // the other way round in the log, the same sides: qa1 is lower
    view.update(cx, |view, cx| view.click_row(3, false, cx));
    view.update(cx, |view, cx| view.click_row(1, true, cx));
    assert_eq!(compared(&view, cx).map(|files| files.len()), Some(3));
    assert!(
        compared(&view, cx)
            .unwrap()
            .contains(&"D qa.txt".to_owned())
    );

    // ⌘-click on the compared row again, or a plain click, ends it
    view.update(cx, |view, cx| view.click_row(1, true, cx));
    assert_eq!(compared(&view, cx), None);
    view.update(cx, |view, cx| view.click_row(4, true, cx));
    assert!(compared(&view, cx).is_some());
    view.update(cx, |view, cx| view.click_row(2, false, cx));
    assert_eq!(compared(&view, cx), None);
    assert_eq!(view.read_with(cx, |view, _| view.selected_row()), Some(2));
}

/// w2 compared with qa1: a reload that keeps w2 selected keeps the
/// comparison, asking again only once w2 is rewritten; New, which
/// leaves the new change selected, ends it rather than compare that change
/// with qa1.
#[gpui::test]
fn a_comparison_ends_when_another_revision_is_selected(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = branch_repo(dir.path());
    let (view, cx) = open(&repo, cx);

    // the log from the working copy: @, w2, w1, qa1, base
    keys(&view, "down", cx);
    view.update(cx, |view, cx| view.click_row(3, true, cx));
    settle(&view, cx);
    let before = view.read_with(cx, |view, _| view.compared_files());
    assert_eq!(before.as_ref().map(Vec::len), Some(3));

    assert_eq!(
        view.read_with(cx, |view, _| view.comparison_queries()),
        Some(1)
    );
    keys(&view, "secondary-r", cx);
    assert_eq!(view.read_with(cx, |view, _| view.compared_files()), before);
    assert_eq!(
        view.read_with(cx, |view, _| view.comparison_queries()),
        Some(1)
    );

    // w2 rewritten outside (a new commit, the same change): asked again
    jj(
        &repo,
        &[
            "--ignore-working-copy",
            "describe",
            "-r",
            "description(exact:\"w2\n\")",
            "-m",
            "w2 again",
        ],
    );
    keys(&view, "secondary-r", cx);
    assert_eq!(view.read_with(cx, |view, _| view.compared_files()), before);
    assert_eq!(
        view.read_with(cx, |view, _| view.comparison_queries()),
        Some(2)
    );

    let w2 = view.read_with(cx, |view, _| view.selected_change());
    keys(&view, "n", cx);
    // the new change on w2 selected, not w2: nothing compared
    assert_ne!(view.read_with(cx, |view, _| view.selected_change()), w2);
    assert_eq!(view.read_with(cx, |view, _| view.compared_files()), None);
}

/// w2 compared with qa1: Squash file from the palette would squash w2's
/// w2.txt, which the comparison hides, so it waits; once Esc ends the
/// comparison it squashes, w2 then changing nothing.
#[gpui::test]
fn file_commands_wait_while_comparing(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = branch_repo(dir.path());
    let (view, cx) = open(&repo, cx);
    let w2 = "description(exact:\"w2\n\")";

    // the log from the working copy: @, w2, w1, qa1, base
    keys(&view, "down", cx);
    view.update(cx, |view, cx| view.click_row(3, true, cx));
    settle(&view, cx);
    assert!(
        view.read_with(cx, |view, _| view.compared_files())
            .is_some()
    );

    cx.dispatch_action(crate::repo_view::actions::SquashFile);
    settle(&view, cx);
    assert_eq!(changed(&repo, w2), "w2.txt");
    assert!(
        view.read_with(cx, |view, _| view.compared_files())
            .is_some()
    );

    keys(&view, "escape", cx);
    cx.dispatch_action(crate::repo_view::actions::SquashFile);
    settle(&view, cx);
    assert_eq!(changed(&repo, w2), "");
}

/// While choosing a revision to rebase w2 onto, ⌘-click on qa1 chooses it, as
/// a click does: no comparison starts.
#[gpui::test]
fn command_click_chooses_while_picking(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = branch_repo(dir.path());
    let (view, cx) = open(&repo, cx);

    // the log from the working copy: @, w2, w1, qa1, base
    keys(&view, "down r", cx);
    view.update(cx, |view, cx| view.click_row(3, true, cx));
    settle(&view, cx);
    assert_eq!(view.read_with(cx, |view, _| view.compared_files()), None);
    assert_eq!(view.read_with(cx, |view, _| view.selected_row()), Some(3));
}

/// A revision adding a.txt, a/y.txt and b/x.txt: as a tree the folders come
/// first, and ↑↓ and ⇧-click go through the files in that order, not the
/// detail's (a.txt, a/y.txt, b/x.txt). Not the working copy, whose ⇧-click
/// ticks boxes.
#[gpui::test]
fn files_as_a_tree_go_in_its_order(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = init(dir.path());
    std::fs::create_dir(repo.join("a")).unwrap();
    std::fs::create_dir(repo.join("b")).unwrap();
    for file in ["a.txt", "a/y.txt", "b/x.txt"] {
        std::fs::write(repo.join(file), "x\n").unwrap();
    }
    jj(&repo, &["commit", "-m", "files"]);
    let (view, cx) = open(&repo, cx);
    // the log from the working copy: @, files
    keys(&view, "down", cx);
    cx.update(|_, cx| crate::settings::update_display(cx, |display| display.file_tree = true));
    settle(&view, cx);

    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["+a/", "  *a/y.txt", "+b/", "  *b/x.txt", "*a.txt"]
    );
    let shown = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| view.shown_file())
    };
    let folder = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| view.file_cursor_folder())
    };
    let list = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, cx| view.file_list(cx))
    };
    // ↑↓ stop on the folders too, the file shown staying
    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));
    keys(&view, "home", cx);
    assert_eq!(folder(&view, cx), Some("a".into()));
    assert_eq!(shown(&view, cx), Some("a/y.txt".into()));
    keys(&view, "down", cx);
    assert_eq!(
        (folder(&view, cx), shown(&view, cx)),
        (None, Some("a/y.txt".into()))
    );
    keys(&view, "down down", cx);
    assert_eq!(
        (folder(&view, cx), shown(&view, cx)),
        (None, Some("b/x.txt".into()))
    );
    keys(&view, "down", cx);
    assert_eq!(shown(&view, cx), Some("a.txt".into()));

    // ← goes to the folder a file is in and closes it, → opens it and
    // goes in; Enter opens and closes it too
    keys(&view, "up", cx);
    keys(&view, "left", cx);
    assert_eq!(folder(&view, cx), Some("b".into()));
    keys(&view, "left", cx);
    assert_eq!(list(&view, cx), ["+a/", "  *a/y.txt", "-b/", "*a.txt"]);
    keys(&view, "right", cx);
    assert_eq!(
        list(&view, cx),
        ["+a/", "  *a/y.txt", "+b/", "  *b/x.txt", "*a.txt"]
    );
    keys(&view, "enter", cx);
    assert_eq!(list(&view, cx), ["+a/", "  *a/y.txt", "-b/", "*a.txt"]);
    assert!(!view.read_with(cx, |view, _| view.full_view_open()));
    keys(&view, "enter right", cx);
    assert_eq!(
        (folder(&view, cx), shown(&view, cx)),
        (None, Some("b/x.txt".into()))
    );
    // a file at the top has no folder to go to: ← does nothing
    keys(&view, "down left", cx);
    assert_eq!(
        (folder(&view, cx), shown(&view, cx)),
        (None, Some("a.txt".into()))
    );
    // ⌘⇧D on a folder still opens the full view of the file shown
    keys(&view, "home secondary-shift-d", cx);
    assert_eq!(
        list(&view, cx),
        ["+a/", "  *a/y.txt", "+b/", "  *b/x.txt", "*a.txt"]
    );
    assert!(view.read_with(cx, |view, _| view.full_view_open()));
    keys(&view, "escape", cx);
    assert!(!view.read_with(cx, |view, _| view.full_view_open()));

    // ⇧ from a/y.txt to a.txt picks the three, as the list shows them
    view.update(cx, |view, cx| {
        view.click_changed("a/y.txt", false, cx);
        view.click_changed("a.txt", true, cx);
    });
    let mut picked = view.read_with(cx, |view, _| view.picked_files());
    picked.sort();
    assert_eq!(picked, ["a.txt", "a/y.txt", "b/x.txt"]);

    // with all files the folders start closed, but the one of the file
    // shown opens
    view.update(cx, |view, cx| view.click_changed("b/x.txt", false, cx));
    cx.update(|_, cx| crate::settings::update_display(cx, |display| display.all_files = true));
    settle(&view, cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["-a/", "+b/", "  *b/x.txt", "*a.txt"]
    );

    // a file shown in paths stays in the list when they turn to a tree
    // (each turn a click of its own: the tabs follow Settings after it)
    view.update(cx, |view, cx| view.set_file_tree(false, cx));
    view.update(cx, |view, cx| view.click_changed("a/y.txt", false, cx));
    view.update(cx, |view, cx| view.set_file_tree(true, cx));
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["+a/", "  *a/y.txt", "+b/", "  *b/x.txt", "*a.txt"]
    );
}

/// A folder ↑↓ are on that the list no longer has, filtered out or of
/// files the revision does not change with all files off: they go on from
/// the file shown.
#[gpui::test]
fn a_folder_out_of_the_list_is_left(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = init(dir.path());
    std::fs::create_dir(repo.join("c")).unwrap();
    std::fs::write(repo.join("c/z.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "c"]);
    std::fs::create_dir(repo.join("a")).unwrap();
    std::fs::write(repo.join("a/y.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "a"]);
    let (view, cx) = open(&repo, cx);
    // the log from the working copy: @, a, c
    keys(&view, "down", cx);
    cx.update(|_, cx| {
        crate::settings::update_display(cx, |display| {
            display.file_tree = true;
            display.all_files = true;
        })
    });
    settle(&view, cx);
    let state = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| (view.file_cursor_folder(), view.shown_file()))
    };
    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));
    keys(&view, "end", cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["+a/", "  *a/y.txt", "-c/"]
    );
    assert_eq!(state(&view, cx), (Some("c".into()), Some("a/y.txt".into())));

    // typed, as the filter's events come
    view.update_in(cx, |view, window, cx| view.focus_file_filter(window, cx));
    keys(&view, "y . t x t", cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["+a/", "  *a/y.txt"]
    );
    assert_eq!(state(&view, cx), (None, Some("a/y.txt".into())));
    view.update_in(cx, |view, window, cx| view.filter_files("", window, cx));

    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));
    keys(&view, "end", cx);
    assert_eq!(state(&view, cx), (Some("c".into()), Some("a/y.txt".into())));
    view.update(cx, |view, cx| view.set_all_files(false, cx));
    settle(&view, cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["+a/", "  *a/y.txt"]
    );
    assert_eq!(state(&view, cx), (None, Some("a/y.txt".into())));
    // ↑ from the file shown, not from the end
    keys(&view, "up", cx);
    assert_eq!(state(&view, cx), (Some("a".into()), Some("a/y.txt".into())));
}

/// Path or Tree and All files turned in another tab: this one follows as
/// if they were turned here.
#[gpui::test]
fn the_file_list_follows_another_tab(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = init(dir.path());
    std::fs::create_dir(repo.join("c")).unwrap();
    std::fs::write(repo.join("c/z.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "c"]);
    std::fs::create_dir(repo.join("a")).unwrap();
    std::fs::write(repo.join("a/y.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "a"]);
    let (view, cx) = open(&repo, cx);
    let other = open_beside(&repo, cx);
    // the log from the working copy: @, a, c
    keys(&view, "down", cx);
    other.update(cx, |other, cx| other.set_file_tree(true, cx));
    other.update(cx, |other, cx| other.set_all_files(true, cx));
    settle(&view, cx);
    let state = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| (view.file_cursor_folder(), view.shown_file()))
    };
    let list = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, cx| view.file_list(cx))
    };
    assert_eq!(list(&view, cx), ["+a/", "  *a/y.txt", "-c/"]);

    // to paths: the folder ↑↓ were on goes
    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));
    keys(&view, "end", cx);
    assert_eq!(state(&view, cx), (Some("c".into()), Some("a/y.txt".into())));
    other.update(cx, |other, cx| other.set_file_tree(false, cx));
    settle(&view, cx);
    assert_eq!(list(&view, cx), ["*a/y.txt", "c/z.txt"]);
    assert_eq!(state(&view, cx), (None, Some("a/y.txt".into())));

    // back to a tree: the folder of the file shown opens
    view.update(cx, |view, cx| view.click_all_folders(false, cx));
    other.update(cx, |other, cx| other.set_file_tree(true, cx));
    settle(&view, cx);
    assert_eq!(list(&view, cx), ["+a/", "  *a/y.txt", "-c/"]);

    // All files off: the unchanged file shown and the folder ↑↓ were on
    // go, the folders start as a new list (here a/ closed, c/ opened)
    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));
    keys(&view, "end right home left", cx);
    view.update_in(cx, |view, window, cx| {
        view.click_unchanged("c/z.txt", window, cx)
    });
    settle(&view, cx);
    keys(&view, "up", cx);
    assert_eq!(list(&view, cx), ["-a/", "+c/", "  c/z.txt"]);
    assert_eq!(
        view.read_with(cx, |view, _| view.file_cursor_folder()),
        Some("c".into())
    );
    other.update(cx, |other, cx| other.set_all_files(false, cx));
    settle(&view, cx);
    assert_eq!(list(&view, cx), ["+a/", "  *a/y.txt"]);
    assert_eq!(view.read_with(cx, |view, _| view.shown_unchanged()), None);
    assert_eq!(state(&view, cx), (None, Some("a/y.txt".into())));
}

/// All files turned on again for a revision whose files came before, and
/// Home and End from a file shown in a closed folder.
#[gpui::test]
fn the_file_shown_stays_in_a_list_of_all_files_again(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = init(dir.path());
    std::fs::write(repo.join("b.txt"), "x\n").unwrap();
    std::fs::create_dir(repo.join("a")).unwrap();
    std::fs::write(repo.join("a/y.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "a"]);
    let (view, cx) = open(&repo, cx);
    let list = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, cx| view.file_list(cx))
    };
    let state = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| (view.file_cursor_folder(), view.shown_file()))
    };
    // the log from the working copy: @, a
    keys(&view, "down", cx);
    view.update(cx, |view, cx| view.set_file_tree(true, cx));
    view.update(cx, |view, cx| view.click_changed("a/y.txt", false, cx));
    view.update(cx, |view, cx| view.set_all_files(true, cx));
    settle(&view, cx);
    assert_eq!(list(&view, cx), ["+a/", "  *a/y.txt", "*b.txt"]);
    view.update(cx, |view, cx| view.set_all_files(false, cx));
    view.update(cx, |view, cx| view.set_all_files(true, cx));
    settle(&view, cx);
    assert_eq!(list(&view, cx), ["+a/", "  *a/y.txt", "*b.txt"]);

    // Home and End from a/y.txt in a closed a/: to the ends
    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));
    view.update(cx, |view, cx| view.click_all_folders(false, cx));
    assert_eq!(list(&view, cx), ["-a/", "*b.txt"]);
    keys(&view, "end", cx);
    assert_eq!(state(&view, cx), (None, Some("b.txt".into())));
    view.update(cx, |view, cx| view.click_changed("a/y.txt", false, cx));
    view.update(cx, |view, cx| view.click_all_folders(false, cx));
    assert_eq!(list(&view, cx), ["-a/", "*b.txt"]);
    keys(&view, "home", cx);
    assert_eq!(state(&view, cx), (Some("a".into()), Some("a/y.txt".into())));
}

/// A filter typed after folders were closed: the matches show, their
/// folders open; emptied, the folders close again but the shown file's.
#[gpui::test]
fn a_filter_opens_the_folders_of_its_matches(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = init(dir.path());
    std::fs::create_dir(repo.join("a")).unwrap();
    std::fs::write(repo.join("a/y.txt"), "x\n").unwrap();
    std::fs::write(repo.join("a/z.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "a"]);
    std::fs::write(repo.join("b.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "b"]);
    let (view, cx) = open(&repo, cx);
    let list = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, cx| view.file_list(cx))
    };
    // the log from the working copy: @, b, a
    keys(&view, "down", cx);
    cx.update(|_, cx| {
        crate::settings::update_display(cx, |display| {
            display.file_tree = true;
            display.all_files = true;
        })
    });
    settle(&view, cx);
    assert_eq!(list(&view, cx), ["-a/", "*b.txt"]);

    // closed by Collapse all, then filtered
    view.update(cx, |view, cx| view.click_all_folders(false, cx));
    view.update_in(cx, |view, window, cx| view.focus_file_filter(window, cx));
    keys(&view, "z", cx);
    assert_eq!(list(&view, cx), ["+a/", "  a/z.txt"]);
    // closed while filtering: as clicked
    view.update(cx, |view, cx| view.click_all_folders(false, cx));
    assert_eq!(list(&view, cx), ["-a/"]);
    keys(&view, "backspace", cx);
    assert_eq!(list(&view, cx), ["-a/", "*b.txt"]);

    // opened while filtering, then emptied: closed again
    keys(&view, "y", cx);
    view.update(cx, |view, cx| view.click_all_folders(true, cx));
    keys(&view, "backspace", cx);
    assert_eq!(list(&view, cx), ["-a/", "*b.txt"]);

    // a file shown in a folder closed while filtering: its folder opens
    keys(&view, "z", cx);
    view.update_in(cx, |view, window, cx| {
        view.click_unchanged("a/z.txt", window, cx)
    });
    view.update(cx, |view, cx| view.click_all_folders(false, cx));
    assert_eq!(list(&view, cx), ["-a/"]);
    view.update_in(cx, |view, window, cx| view.focus_file_filter(window, cx));
    keys(&view, "backspace", cx);
    assert_eq!(list(&view, cx), ["+a/", "  a/y.txt", "  a/z.txt", "*b.txt"]);
}

/// A comparison's folders are its own: each comparison starts with them
/// open whatever the inspector's are, and the inspector's stay as they
/// were whatever is opened or closed in it.
#[gpui::test]
fn a_comparison_has_folders_of_its_own(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = init(dir.path());
    std::fs::create_dir(repo.join("a")).unwrap();
    std::fs::write(repo.join("a/y.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "a"]);
    std::fs::create_dir(repo.join("c")).unwrap();
    std::fs::write(repo.join("c/z.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "c"]);
    let (view, cx) = open(&repo, cx);
    let list = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, cx| view.file_list(cx))
    };
    let compared = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        settle(view, cx);
        view.read_with(cx, |view, cx| view.compared_list(cx))
    };
    // the log from the working copy: @, c, a
    keys(&view, "down", cx);
    view.update(cx, |view, cx| view.set_file_tree(true, cx));
    settle(&view, cx);
    assert_eq!(list(&view, cx), ["+c/", "  *c/z.txt"]);

    // closed in the inspector, open in a comparison of c with a
    view.update(cx, |view, cx| view.click_all_folders(false, cx));
    assert_eq!(list(&view, cx), ["-c/"]);
    view.update(cx, |view, cx| view.click_row(2, true, cx));
    assert_eq!(
        compared(&view, cx),
        Some(vec!["+c/".to_owned(), "  *c/z.txt".to_owned()])
    );
    keys(&view, "escape", cx);
    assert_eq!(compared(&view, cx), None);

    // closed in a comparison, open in the inspector after it
    view.update(cx, |view, cx| view.click_all_folders(true, cx));
    view.update(cx, |view, cx| view.click_row(2, true, cx));
    assert!(compared(&view, cx).is_some());
    view.update(cx, |view, cx| view.click_all_compared_folders(false, cx));
    assert_eq!(compared(&view, cx), Some(vec!["-c/".to_owned()]));
    keys(&view, "escape", cx);
    assert_eq!(compared(&view, cx), None);
    assert_eq!(list(&view, cx), ["+c/", "  *c/z.txt"]);
}

/// A revision that changes nothing, with all files: Enter in the revisions
/// goes to its files all the same, ↓ to the first.
#[gpui::test]
fn enter_goes_to_the_files_of_a_revision_with_no_changes(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = init(dir.path());
    std::fs::write(repo.join("y.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "y"]);
    jj(&repo, &["commit", "-m", "nothing"]);
    let (view, cx) = open(&repo, cx);
    // the log from the working copy: @, nothing, y
    keys(&view, "down", cx);
    view.update(cx, |view, cx| view.set_all_files(true, cx));
    settle(&view, cx);
    assert_eq!(view.read_with(cx, |view, cx| view.file_list(cx)), ["y.txt"]);
    keys(&view, "enter down", cx);
    settle(&view, cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.shown_unchanged()),
        Some(("y.txt".to_owned(), None))
    );
}

/// ↑↓ and the full view's next and previous file from a file shown in a
/// closed folder: on from that folder's row, the file just below it.
#[gpui::test]
fn arrows_go_on_from_a_closed_folder(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = init(dir.path());
    for folder in ["a", "b"] {
        std::fs::create_dir(repo.join(folder)).unwrap();
    }
    std::fs::write(repo.join("a/y.txt"), "x\n").unwrap();
    std::fs::write(repo.join("b/x.txt"), "x\n").unwrap();
    std::fs::write(repo.join("c.txt"), "x\n").unwrap();
    jj(&repo, &["commit", "-m", "abc"]);
    let (view, cx) = open(&repo, cx);
    let state = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| (view.file_cursor_folder(), view.shown_file()))
    };
    // the file shown, then every folder closed over it
    let hide = |view: &Entity<RepoView>, path: &str, cx: &mut VisualTestContext| {
        view.update(cx, |view, cx| view.click_changed(path, false, cx));
        view.update(cx, |view, cx| view.click_all_folders(false, cx));
        assert_eq!(
            view.read_with(cx, |view, cx| view.file_list(cx)),
            ["-a/", "-b/", "*c.txt"]
        );
    };
    // the log from the working copy: @, abc
    keys(&view, "down", cx);
    view.update(cx, |view, cx| view.set_file_tree(true, cx));
    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));

    hide(&view, "b/x.txt", cx);
    keys(&view, "down", cx);
    assert_eq!(state(&view, cx), (None, Some("c.txt".into())));
    hide(&view, "b/x.txt", cx);
    keys(&view, "up", cx);
    assert_eq!(state(&view, cx), (Some("b".into()), Some("b/x.txt".into())));
    hide(&view, "a/y.txt", cx);
    keys(&view, "up", cx);
    assert_eq!(state(&view, cx), (Some("a".into()), Some("a/y.txt".into())));
    // ← as ↑: to the folder the file shown is in
    hide(&view, "b/x.txt", cx);
    keys(&view, "left", cx);
    assert_eq!(state(&view, cx), (Some("b".into()), Some("b/x.txt".into())));
    hide(&view, "a/y.txt", cx);

    // the full view stops at changed files: none before a/
    keys(&view, "secondary-shift-d", cx);
    assert!(view.read_with(cx, |view, _| view.full_view_open()));
    view.update(cx, |view, cx| view.click_all_folders(false, cx));
    keys(&view, "[", cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.shown_file()),
        Some("a/y.txt".into())
    );
    keys(&view, "]", cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.shown_file()),
        Some("c.txt".into())
    );
}

/// The working copy as a tree: a folder's box includes every file under
/// it in the next commit, and says how much of them is.
#[gpui::test]
fn a_folder_s_box_includes_its_files(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = init(dir.path());
    std::fs::create_dir_all(repo.join("a/c")).unwrap();
    for file in ["a/1.txt", "a/c/2.txt", "b.txt"] {
        std::fs::write(repo.join(file), "x\n").unwrap();
    }
    jj(&repo, &["status"]);
    let (view, cx) = open(&repo, cx);
    cx.update(|_, cx| crate::settings::update_display(cx, |display| display.file_tree = true));
    settle(&view, cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["+a/", "  +c/", "    *a/c/2.txt", "  *a/1.txt", "*b.txt"]
    );
    let boxes = |view: &Entity<RepoView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| (view.folder_box("a"), view.folder_box("a/c")))
    };
    assert_eq!(boxes(&view, cx), ("none", "none"));
    view.update(cx, |view, cx| view.click_folder_box("a/c", cx));
    assert_eq!(boxes(&view, cx), ("some", "all"));
    view.update(cx, |view, cx| view.click_folder_box("a", cx));
    assert_eq!(boxes(&view, cx), ("all", "all"));

    keys(&view, "d f i x", cx);
    keys(&view, "secondary-enter", cx);
    let repo = repo.as_path();
    assert_eq!(changed(repo, "@-"), "a/1.txt a/c/2.txt");
    assert_eq!(changed(repo, "@"), "b.txt");

    // the folder ↑↓ are on gone in a reload: they are on it no more
    std::fs::create_dir(repo.join("d")).unwrap();
    std::fs::write(repo.join("d/e.txt"), "x\n").unwrap();
    jj(repo, &["status"]);
    // the commit stays selected: the working copy, at the top
    view.update_in(cx, |view, window, cx| view.focus_log(window, cx));
    keys(&view, "secondary-r home", cx);
    settle(&view, cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["+d/", "  *d/e.txt", "*b.txt"]
    );
    view.update_in(cx, |view, window, cx| view.focus_files(window, cx));
    keys(&view, "home", cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.file_cursor_folder()),
        Some("d".into())
    );
    std::fs::remove_dir_all(repo.join("d")).unwrap();
    jj(repo, &["status"]);
    keys(&view, "secondary-r", cx);
    settle(&view, cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["*b.txt"]
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.file_cursor_folder()),
        None
    );
}

/// w2 with all files: base.txt and w1.txt, which it does not change, are
/// listed with w2.txt; one clicked shows its text, the filter keeps the
/// paths with its text, the working copy lists only what it changes, and
/// without all files the changed file is shown again.
#[gpui::test]
fn all_files_list_and_show_the_unchanged_ones(cx: &mut TestAppContext) {
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = branch_repo(dir.path());
    let (view, cx) = open(&repo, cx);

    // the log from the working copy: @, w2, w1, qa1, base
    keys(&view, "down", cx);
    cx.update(|_, cx| crate::settings::update_display(cx, |display| display.all_files = true));
    settle(&view, cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["base.txt", "w1.txt", "*w2.txt"]
    );

    // from the full view of w2.txt: it closes, the text shows below
    keys(&view, "secondary-shift-d", cx);
    assert!(view.read_with(cx, |view, _| view.full_view_open()));
    view.update_in(cx, |view, window, cx| {
        view.click_unchanged("w1.txt", window, cx)
    });
    settle(&view, cx);
    assert!(!view.read_with(cx, |view, _| view.full_view_open()));
    assert_eq!(
        view.read_with(cx, |view, _| view.shown_unchanged()),
        Some(("w1.txt".to_owned(), None))
    );
    assert_eq!(view.read_with(cx, |view, _| view.shown_file()), None);

    // a comparison started and ended (⌘-click on qa1, twice) leaves it shown
    view.update(cx, |view, cx| view.click_row(3, true, cx));
    settle(&view, cx);
    view.update(cx, |view, cx| view.click_row(3, true, cx));
    settle(&view, cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.shown_unchanged()),
        Some(("w1.txt".to_owned(), None))
    );

    view.update_in(cx, |view, window, cx| view.filter_files("W1", window, cx));
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["w1.txt"]
    );
    view.update_in(cx, |view, window, cx| view.filter_files("", window, cx));

    // the working copy, the next commit, lists what it changes: nothing
    view.update_in(cx, |view, window, cx| view.focus_log(window, cx));
    keys(&view, "up", cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        Vec::<String>::new()
    );
    keys(&view, "down", cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["base.txt", "w1.txt", "*w2.txt"]
    );

    view.update(cx, |view, cx| view.set_all_files(false, cx));
    settle(&view, cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.file_list(cx)),
        ["*w2.txt"]
    );
    assert_eq!(view.read_with(cx, |view, _| view.shown_unchanged()), None);
    assert_eq!(
        view.read_with(cx, |view, _| view.shown_file()),
        Some("w2.txt".into())
    );
}

/// The message box's handle under a description shorter than its limit
/// (three lines of eight): dragged up, the box shrinks at once; dragged
/// down, it has no more to show, so the limit stays where it was until the
/// drag passes it. Read-only (a tagged revision), edited and in the commit
/// box alike.
#[gpui::test]
fn a_short_description_s_box_follows_its_handle(cx: &mut TestAppContext) {
    use crate::repo_view::MessageBox;
    if !has_jj() {
        eprintln!("skipped: no jj on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = init(dir.path());
    for (file, message) in [
        ("base.txt", "base\n\none\ntwo\nthree"),
        ("work.txt", "work\n\nfirst\nsecond\nthird"),
    ] {
        std::fs::write(repo.join(file), "x\n").unwrap();
        jj(&repo, &["commit", "-m", message]);
    }
    jj(&repo, &["tag", "set", "-r", "@--", "v1"]);
    let (view, cx) = open(&repo, cx);
    let saved = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| crate::settings::get(cx).display.message_lines)
    };
    let drag = |view: &Entity<RepoView>, which, dys: &[f32], cx: &mut VisualTestContext| {
        let lines = view.update_in(cx, |view, window, cx| {
            view.drag_message_handle(which, dys, window, cx)
        });
        settle(view, cx);
        lines
    };
    let shown = |view: &Entity<RepoView>, which, cx: &mut VisualTestContext| {
        view.update_in(cx, |view, window, cx| {
            view.message_box_lines(which, window, cx)
        })
    };
    // a line is 1.25rem: 20 pixels in a test window
    assert_eq!(saved(cx), 8);

    // the working copy's commit box: D, a summary, then three lines
    keys(&view, "d s enter a enter b enter c", cx);
    assert_eq!(shown(&view, MessageBox::Commit, cx), Some(3));
    assert_eq!(drag(&view, MessageBox::Commit, &[-20., 0.], cx), [2, 8]);
    assert_eq!(saved(cx), 8);

    // the log from the working copy: @, work, base
    view.update_in(cx, |view, window, cx| view.focus_log(window, cx));
    keys(&view, "down down", cx);
    assert_eq!(shown(&view, MessageBox::ReadOnly, cx), Some(3));
    assert_eq!(
        drag(&view, MessageBox::ReadOnly, &[-20., 0., 60., 140., 0.], cx),
        [2, 8, 8, 10, 8]
    );
    assert_eq!(saved(cx), 8, "let go where it started, nothing changes");
    assert_eq!(shown(&view, MessageBox::ReadOnly, cx), Some(3));

    keys(&view, "up", cx);
    assert_eq!(shown(&view, MessageBox::Description, cx), Some(3));
    assert_eq!(drag(&view, MessageBox::Description, &[-20.], cx), [2]);
    assert_eq!(saved(cx), 2);
    assert_eq!(shown(&view, MessageBox::Description, cx), Some(2));
}
