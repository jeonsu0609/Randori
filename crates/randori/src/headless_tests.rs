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
