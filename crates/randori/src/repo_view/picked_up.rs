//! Files a move of the working copy picked up: moving to a revision that does
//! not ignore files the previous one ignored (an older `.gitignore`) makes the
//! next snapshot track them, often a whole build folder (jj #5596). After New,
//! Edit or a bookmark switch the tab snapshots once and, when the new working
//! copy gained files, says so and offers to ignore them on this computer.

use gg_lib::messages::mutations::IgnoreNewFiles;
use gg_lib::messages::queries::ChangeKind;
use gpui_kit::component::{WindowExt as _, notification::Notification};

use super::*;

/// Paths named in the notification; the rest are counted.
const NAMED: usize = 3;

impl RepoView {
    /// Call before a mutation that moves the working copy; the check runs
    /// after the reload that follows it.
    pub(super) fn check_picked_up_after(&mut self) {
        // the working copy before the move: if it is still the same when the
        // check runs, the move did not happen (refused, failed) and there is
        // nothing to look for
        self.check_picked_up = Some(self.working_copy_change());
    }

    fn working_copy_change(&self) -> Option<String> {
        self.rows
            .iter()
            .find(|row| row.revision.is_working_copy)
            .map(|row| row.revision.id.change.hex.clone())
    }

    /// From `replace_rows`: runs the check a move asked for.
    pub(super) fn run_picked_up_check(&mut self, cx: &mut Context<Self>) {
        // wait for the last mutation's reload, keeping the request until then
        if self.check_picked_up.is_none() || !self.busy.is_empty() {
            return;
        }
        let before = self.check_picked_up.take().flatten();
        let moved = before.is_none() || before != self.working_copy_change();
        // "Never snapshot" means no snapshot here either
        if !moved || settings::get(cx).auto_snapshot == Some(false) {
            return;
        }
        let worker = self.worker.clone();
        self.picked_up_checking = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    // files the working copy adds on its own (an edited commit's)
                    // are not picked up; only what the snapshot brings in is
                    let before = added_in_working_copy(&worker)?;
                    if worker.snapshot(true)?.is_none() {
                        return anyhow::Ok(Vec::new());
                    }
                    let mut after = added_in_working_copy(&worker)?;
                    after.retain(|path| {
                        !before.iter().any(|known| known.repo_path == path.repo_path)
                    });
                    Ok(after)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.picked_up_checking = false;
                match result {
                    Ok(paths) if !paths.is_empty() => {
                        view.picked_up = paths;
                        view.reload(cx);
                    }
                    Ok(_) => {}
                    Err(err) => log::warn!("checking picked-up files: {err:#}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Shows the notification for files found by the check, once the tab is
    /// drawn with a window to show it in.
    pub(super) fn notify_picked_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picked_up.is_empty() {
            return;
        }
        let paths = std::mem::take(&mut self.picked_up);
        let named: Vec<&str> = paths
            .iter()
            .take(NAMED)
            .map(|path| path.repo_path.as_str())
            .collect();
        let more = paths.len().saturating_sub(NAMED);
        let list = if more > 0 {
            format!("{} and {more} more", named.join(", "))
        } else {
            named.join(", ")
        };
        let count = paths.len();
        let view = cx.entity().downgrade();
        let paths = Rc::new(paths);
        window.push_notification(
            Notification::warning(format!(
                "{list} were ignored where you came from. Ignore keeps them on disk \
                 and adds them to .git/info/exclude."
            ))
            .title(format!(
                "The new working copy picked up {count} file{}",
                if count == 1 { "" } else { "s" }
            ))
            .action(move |_, _, cx| {
                let (view, paths) = (view.clone(), paths.clone());
                let notification = cx.entity().downgrade();
                Button::new("ignore-picked-up")
                    .primary()
                    .xsmall()
                    .label("Ignore locally")
                    .on_click(move |_, window, cx| {
                        let paths = (*paths).clone();
                        let _ = view.update(cx, |view, cx| {
                            view.run_mutation(
                                format!("Ignore {} file(s) locally", paths.len()),
                                Box::new(IgnoreNewFiles { paths }),
                                None,
                                window,
                                cx,
                            )
                        });
                        let _ = notification.update(cx, |n, cx| n.dismiss(window, cx));
                    })
            }),
            cx,
        );
    }

    pub(super) fn picked_up_pending(&self) -> bool {
        self.check_picked_up.is_some() || self.picked_up_checking || !self.picked_up.is_empty()
    }
}

/// Files the working-copy commit adds compared with its parent.
fn added_in_working_copy(worker: &crate::worker::Worker) -> anyhow::Result<Vec<TreePath>> {
    let Some(wc) = worker.query_headers("@".to_owned(), 1)?.into_iter().next() else {
        return Ok(Vec::new());
    };
    let set = RevSet {
        from: wc.id.clone(),
        to: wc.id,
    };
    Ok(match worker.query_revisions(set)? {
        RevsResult::Detail { changes, .. } => changes
            .into_iter()
            .filter(|change| matches!(change.kind, ChangeKind::Added))
            .map(|change| change.path)
            .collect(),
        _ => Vec::new(),
    })
}
