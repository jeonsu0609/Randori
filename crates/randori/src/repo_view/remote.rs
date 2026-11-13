//! Git remotes: pushing bookmarks, fetching (on demand and automatically),
//! and the credentials GG asks for on the way.

use std::collections::HashMap;
use std::rc::Rc;

use gg_lib::messages::mutations::{GitFetch, GitPush, GitRefspec, MutationResult};
use gg_lib::messages::{InputRequest, InputResponse, StoreRef};
use gpui_kit::component::{
    WindowExt as _,
    button::Button,
    dialog::{DialogAction, DialogClose, DialogFooter},
    input::{Input, InputState},
    notification::Notification,
    v_flex,
};
use gpui_kit::{Context, Entity, SharedString, Window, div};

use super::RepoView;
use super::ops::Retry;

use super::*;

impl RepoView {
    /// Pushes one local bookmark to every remote it tracks (or the default remote).
    pub(super) fn push_ref_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(r#ref @ StoreRef::LocalBookmark { .. }) = self.ref_target.clone() {
            self.push_bookmarks(vec![r#ref], window, cx);
        }
    }

    /// Pushes the local bookmarks that point at the selected revision.
    pub(super) fn push_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(header) = self.selected_header() else {
            return;
        };
        let bookmarks: Vec<StoreRef> = header
            .refs
            .iter()
            .filter(|r| matches!(r, StoreRef::LocalBookmark { .. }))
            .cloned()
            .collect();
        if bookmarks.is_empty() {
            window.push_notification(
                Notification::info("Create a bookmark on this revision to push it.")
                    .title("Nothing to push"),
                cx,
            );
            return;
        }
        self.push_bookmarks(bookmarks, window, cx);
    }

    fn push_bookmarks(
        &mut self,
        bookmarks: Vec<StoreRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remotes = self
            .workspace
            .as_ref()
            .map(|ws| ws.remotes.clone())
            .unwrap_or_default();
        for bookmark_ref in bookmarks {
            let name = bookmark_ref.as_bookmark().unwrap_or_default().to_owned();
            // A tracked bookmark goes to the remotes it tracks. An untracked one goes
            // to `origin` (or the only remote), which also starts tracking it.
            let tracked = matches!(
                &bookmark_ref,
                StoreRef::LocalBookmark { tracking_remotes, .. } if !tracking_remotes.is_empty()
            );
            let target = if tracked {
                None
            } else {
                default_remote(&remotes)
            };
            if !tracked && target.is_none() {
                window.push_notification(
                    Notification::info("Add a git remote to push this bookmark.")
                        .title(format!("Push {name}")),
                    cx,
                );
                continue;
            }
            let refspec = move |bookmark_ref: StoreRef| match &target {
                Some(remote_name) => GitRefspec::RemoteBookmark {
                    remote_name: remote_name.clone(),
                    bookmark_ref,
                },
                None => GitRefspec::AllRemotes { bookmark_ref },
            };
            let label = match refspec(bookmark_ref.clone()) {
                GitRefspec::RemoteBookmark { remote_name, .. } => {
                    format!("Push {name} to {remote_name}")
                }
                _ => format!("Push {name}"),
            };
            let refspec = Rc::new(refspec);
            let retry_ref = bookmark_ref.clone();
            let retry_spec = refspec.clone();
            let retry: Retry = Rc::new(move |input| {
                Box::new(GitPush {
                    refspec: retry_spec(retry_ref.clone()),
                    input: Some(input),
                })
            });
            self.run_mutation(
                label,
                Box::new(GitPush {
                    refspec: refspec(bookmark_ref),
                    input: None,
                }),
                Some(retry),
                window,
                cx,
            );
        }
    }

    /// Settings' "Fetch automatically": while it is on, a look every minute
    /// whether the interval has passed since the last automatic fetch; while
    /// it is off, nothing runs.
    pub(super) fn follow_auto_fetch(&mut self, cx: &mut Context<Self>) {
        let on = crate::settings::get(cx).display.auto_fetch_mins > 0;
        if on == self.auto_fetch.is_some() {
            return;
        }
        if !on {
            self.auto_fetch = None;
            return;
        }
        self.auto_fetch = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(60))
                    .await;
                if this
                    .update(cx, |view, cx| view.auto_fetch_if_due(cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Fetches every remote quietly when due: no progress, no password
    /// prompt, and a failure only logged, since nobody asked for it. Skipped
    /// while the tab's own commands run.
    fn auto_fetch_if_due(&mut self, cx: &mut Context<Self>) {
        let mins = crate::settings::get(cx).display.auto_fetch_mins;
        let due = std::time::Duration::from_secs(u64::from(mins) * 60);
        if mins == 0 || !self.busy.is_empty() || self.last_auto_fetch.elapsed() < due {
            return;
        }
        let remotes = self
            .workspace
            .as_ref()
            .map(|ws| ws.remotes.clone())
            .unwrap_or_default();
        if remotes.is_empty() {
            return;
        }
        self.last_auto_fetch = std::time::Instant::now();
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let status = cx
                .background_spawn(async move {
                    let mut status = None;
                    for remote_name in remotes {
                        let fetch = GitFetch {
                            refspec: GitRefspec::AllBookmarks {
                                remote_name: remote_name.clone(),
                            },
                            input: None,
                        };
                        match worker.mutate(Box::new(fetch)) {
                            Ok(MutationResult::Updated { new_status, .. }) => {
                                status = Some(new_status)
                            }
                            Ok(MutationResult::Unchanged) => {}
                            Ok(_) => log::info!("automatic fetch of {remote_name}: skipped"),
                            Err(err) => log::info!("automatic fetch of {remote_name}: {err:#}"),
                        }
                    }
                    status
                })
                .await;
            if let Some(status) = status {
                let _ = this.update(cx, |view, cx| {
                    view.apply_status(&status, cx);
                    view.reload(cx);
                });
            }
        })
        .detach();
    }

    /// Fetches every bookmark from every git remote of the workspace.
    pub(super) fn fetch_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let remotes = self
            .workspace
            .as_ref()
            .map(|ws| ws.remotes.clone())
            .unwrap_or_default();
        if remotes.is_empty() {
            window.push_notification(
                Notification::info("This workspace has no git remotes.").title("Nothing to fetch"),
                cx,
            );
            return;
        }
        for remote_name in remotes {
            let retry_remote = remote_name.clone();
            let retry: Retry = Rc::new(move |input| {
                Box::new(GitFetch {
                    refspec: GitRefspec::AllBookmarks {
                        remote_name: retry_remote.clone(),
                    },
                    input: Some(input),
                })
            });
            self.run_mutation(
                format!("Fetch {remote_name}"),
                Box::new(GitFetch {
                    refspec: GitRefspec::AllBookmarks { remote_name },
                    input: None,
                }),
                Some(retry),
                window,
                cx,
            );
        }
    }

    /// Asks for the fields GG reports for a git operation, then retries it.
    pub(super) fn prompt_credentials(
        &mut self,
        label: SharedString,
        request: InputRequest,
        retry: Retry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fields: Vec<(String, Entity<InputState>)> = request
            .fields
            .iter()
            .map(|field| {
                let secret = field.label.to_lowercase().contains("pass")
                    || field.label.to_lowercase().contains("token");
                let state = cx.new(|cx| InputState::new(window, cx).masked(secret));
                (field.label.clone(), state)
            })
            .collect();
        if let Some((_, first)) = fields.first() {
            let first = first.clone();
            window.defer(cx, move |window, cx| {
                first.update(cx, |state, cx| state.focus(window, cx));
            });
        }
        let view = cx.entity().downgrade();
        let title = request.title.clone();
        let detail = request.detail.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let retry = retry.clone();
            let label = label.clone();
            let fields_for_ok = fields.clone();
            dialog
                .title(title.clone())
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(detail.clone()))
                        .children(fields.iter().map(|(name, state)| {
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child(name.clone()))
                                .child(Input::new(state))
                        })),
                )
                .footer(
                    DialogFooter::new()
                        .gap_2()
                        .child(
                            DialogClose::new()
                                .child(Button::new("cancel").label("Cancel").outline()),
                        )
                        .child(
                            DialogAction::new()
                                .child(Button::new("ok").label("Continue").primary()),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    let values: HashMap<String, String> = fields_for_ok
                        .iter()
                        .map(|(name, state)| (name.clone(), state.read(cx).value().to_string()))
                        .collect();
                    let mutation = retry(InputResponse { fields: values });
                    let label = label.clone();
                    let _ = view.update(cx, |view, cx| {
                        view.run_mutation(label, mutation, None, window, cx)
                    });
                    true
                })
        });
    }
}

/// `origin` if the workspace has it, otherwise the first remote.
fn default_remote(remotes: &[String]) -> Option<String> {
    remotes
        .iter()
        .find(|remote| remote.as_str() == "origin")
        .or_else(|| remotes.first())
        .cloned()
}
