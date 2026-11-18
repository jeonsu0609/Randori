//! What the Overview writes to GitHub, as GitKraken's pull request view does:
//! the merge box under the conversation, the comment box under it, and
//! "Submit a review" atop the side column. One request goes at a time; when
//! it is answered the pull request and the list reload, and a refusal (a
//! conflict, no permission, a push since the page loaded) is shown above the
//! comment box.

use std::cell::Cell;
use std::future::Future;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::DropdownButton;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::{Radio, RadioGroup};
use http_client::HttpClient;

use super::*;
use crate::harness_mark::HarnessMark as _;
use gh::{MergeMethod, Mergeability, ReviewEvent};

/// What is on its way, on the button that sent it.
const MERGING: &str = "Merging…";
const COMMENTING: &str = "Commenting…";
const REVIEWING: &str = "Reviewing…";

/// What was done, said once the pull request's view has closed.
fn done_words(busy: &str) -> &'static str {
    match busy {
        MERGING => "Merged.",
        COMMENTING => "Comment added.",
        _ => "Review submitted.",
    }
}

/// A review's choices, in GitHub's order, with their words.
const REVIEW_CHOICES: [(ReviewEvent, &str); 3] = [
    (ReviewEvent::Comment, "Comment"),
    (ReviewEvent::Approve, "Approve"),
    (ReviewEvent::RequestChanges, "Request changes"),
];

impl PullComposer {
    /// The way the merge button merges: the one chosen, while the repository
    /// allows it, or else the first it allows.
    fn merge_method(&self) -> Option<MergeMethod> {
        let methods = &self.detail.merge.methods;
        self.method
            .filter(|method| methods.contains(method))
            .or_else(|| methods.first().copied())
    }

    /// Sends what `request` makes of the client, repository and token, with
    /// `busy` on the buttons meanwhile; either way the pull request and the
    /// list reload (a refusal may be news too: merged elsewhere, a push).
    fn send<F>(
        &mut self,
        busy: &'static str,
        request: impl FnOnce(Arc<dyn HttpClient>, github::GitHubRepo, String) -> F,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        F: Future<Output = Result<(), FetchError>> + Send + 'static,
    {
        if self.busy.is_some() {
            return;
        }
        let (repo, token) = self.access.clone();
        let request = request(cx.http_client(), repo, token);
        self.busy = Some(busy);
        self.error = None;
        cx.notify();
        let executor = cx.background_executor().clone();
        let (owner, number) = (self.owner.clone(), self.detail.number);
        // A write goes to its end: closing the pull request while it is on
        // its way must not cancel it half sent, as dropping the task would.
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(within(executor, request))
                .await
                .map_err(fetch_message);
            let shown = this.update_in(cx, |view, window, cx| {
                view.busy = None;
                let sent = view.pending_comment.take();
                match &result {
                    // the box empties, unless more was typed meanwhile
                    Ok(()) => {
                        if let Some(sent) = sent
                            && view.comment.read(cx).value().trim() == sent
                        {
                            view.comment
                                .update(cx, |state, cx| state.set_value("", window, cx));
                        }
                    }
                    Err(message) => view.error = Some(message.clone().into()),
                }
                cx.emit(OverviewEvent::Changed);
                cx.notify();
            });
            // closed meanwhile: the tab says how it went
            if shown.is_err() {
                let _ = cx.update(|window, cx| {
                    let note = match result {
                        Ok(()) => Notification::info(done_words(busy)),
                        Err(message) => Notification::warning(message),
                    }
                    .title(format!("Pull request #{number}"));
                    window.push_notification(note, cx);
                    let _ = owner.update(cx, |view, cx| view.refresh_pulls(true, cx));
                });
            }
        })
        .detach();
    }

    /// Asks before merging: what goes where, and whether the head branch is
    /// deleted after (offered when it is this repository's and GitHub does
    /// not delete it by itself).
    fn confirm_merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(method) = self.merge_method() else {
            return;
        };
        let detail = self.detail.clone();
        let offer_delete = !detail.merge.deletes_branch && detail.merge.head_ref_id.is_some();
        let delete = Rc::new(Cell::new(false));
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let (view, delete, shown) = (view.clone(), delete.clone(), detail.clone());
            let what = match method {
                MergeMethod::Merge => format!(
                    "The commits of {} go into {} with a merge commit.",
                    detail.head_ref, detail.base_ref
                ),
                MergeMethod::Squash => format!(
                    "The commits of {} go into {} as one commit.",
                    detail.head_ref, detail.base_ref
                ),
                MergeMethod::Rebase => format!(
                    "The commits of {} go onto {} one by one, rebased.",
                    detail.head_ref, detail.base_ref
                ),
            };
            // GitHub deletes only this repository's branch, not a fork's
            let after = if detail.merge.deletes_branch && detail.merge.head_ref_id.is_some() {
                Some(format!(
                    "GitHub then deletes {}, as the repository is set to.",
                    detail.head_ref
                ))
            } else {
                None
            };
            dialog
                .title(format!("{} #{}?", method.verb(), detail.number))
                .child(
                    v_flex()
                        .gap_3()
                        .text_sm()
                        .child(div().child(format!("“{}”", detail.title)))
                        .child(div().child(what))
                        .children(after.map(|after| div().child(after)))
                        .when(offer_delete, |this| {
                            let toggle = delete.clone();
                            this.child(
                                Checkbox::new("pull-merge-delete")
                                    .label(format!("Delete {} on GitHub", detail.head_ref))
                                    .checked(delete.get())
                                    .on_click(move |checked, window, _| {
                                        toggle.set(*checked);
                                        window.refresh();
                                    }),
                            )
                        }),
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
                                .child(Button::new("ok").label(method.verb()).primary()),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    let delete = delete.get();
                    let shown = shown.clone();
                    let _ = view.update(cx, |view, cx| {
                        view.merge(&shown, method, delete, window, cx)
                    });
                    true
                })
        });
    }

    /// Merges the pull request as `shown` when the dialog opened, if its
    /// head is still that one (a reload while the dialog was open does not
    /// change what is merged), and deletes the head branch after when
    /// `delete`.
    fn merge(
        &mut self,
        shown: &PullDetail,
        method: MergeMethod,
        delete: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (id, head) = (shown.id.clone(), shown.head_oid.clone());
        let branch = delete
            .then(|| shown.merge.head_ref_id.clone())
            .flatten()
            .map(|ref_id| (ref_id, shown.head_ref.clone()));
        self.send(
            MERGING,
            move |client, repo, token| async move {
                gh::merge(
                    client.clone(),
                    repo.clone(),
                    token.clone(),
                    id,
                    method,
                    head,
                )
                .await?;
                if let Some((ref_id, name)) = branch {
                    gh::delete_branch(client, repo, token, ref_id)
                        .await
                        .map_err(|err| {
                            FetchError::Other(anyhow::anyhow!(
                                "Merged, but {name} was not deleted: {}",
                                fetch_message(err)
                            ))
                        })?;
                }
                Ok(())
            },
            window,
            cx,
        );
    }

    /// Adds what the comment box holds to the conversation; the box empties
    /// once GitHub has it (unless more was typed meanwhile).
    pub(super) fn submit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.comment.read(cx).value().trim().to_owned();
        if body.is_empty() {
            return;
        }
        if self.busy.is_some() {
            return;
        }
        let id = self.detail.id.clone();
        self.pending_comment = Some(body.clone());
        self.send(
            COMMENTING,
            move |client, repo, token| gh::comment(client, repo, token, id, body),
            window,
            cx,
        );
    }

    /// The review dialog: a message and a verdict. GitHub takes an approval
    /// without words, the others with; the author's own pull request takes
    /// comments only.
    pub(super) fn open_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let own = self.detail.viewer_is_author;
        let event = Rc::new(Cell::new(ReviewEvent::Comment));
        let missing = Rc::new(Cell::new(false));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 12)
                .placeholder("Leave a comment")
        });
        let view = cx.entity().downgrade();
        let field = body.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme();
            let (view, body) = (view.clone(), body.clone());
            let (pick, need) = (event.clone(), missing.clone());
            let (ok_event, ok_missing) = (event.clone(), missing.clone());
            let selected = REVIEW_CHOICES
                .iter()
                .position(|(choice, _)| *choice == event.get());
            dialog
                .title("Submit a review")
                .w(gpui_kit::px(520.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(Textarea::new(&body))
                        .child(
                            RadioGroup::vertical("pull-review-event")
                                .selected_index(selected)
                                .on_click(move |ix, window, _| {
                                    if let Some((choice, _)) = REVIEW_CHOICES.get(*ix) {
                                        pick.set(*choice);
                                        need.set(false);
                                        window.refresh();
                                    }
                                })
                                .children(REVIEW_CHOICES.iter().map(|(choice, label)| {
                                    Radio::new(*label)
                                        .label(*label)
                                        .disabled(own && *choice != ReviewEvent::Comment)
                                })),
                        )
                        .when(own, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        "You opened this pull request: GitHub takes only comments from you.",
                                    ),
                            )
                        })
                        .when(missing.get(), |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.danger)
                                    .child("Write what you think, or what to change."),
                            )
                        }),
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
                                .child(Button::new("ok").label("Submit review").primary()),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    let event = ok_event.get();
                    let text = body.read(cx).value().trim().to_owned();
                    if event.needs_body() && text.is_empty() {
                        ok_missing.set(true);
                        window.refresh();
                        return false;
                    }
                    let _ = view.update(cx, |view, cx| view.review(event, text, window, cx));
                    true
                })
        });
        // the dialog takes focus as it opens; the field has it after
        window.defer(cx, move |window, cx| {
            field.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn review(
        &mut self,
        event: ReviewEvent,
        body: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.detail.id.clone();
        self.send(
            REVIEWING,
            move |client, repo, token| gh::review(client, repo, token, id, event, body),
            window,
            cx,
        );
    }

    /// The merge box, under the conversation of an open pull request: whether
    /// it can be merged, and the button with the way to merge in its menu.
    pub(super) fn merge_box(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let detail = &self.detail;
        if !matches!(detail.state, PullState::Open | PullState::Draft) {
            return None;
        }
        let theme = cx.theme();
        let merge = &detail.merge;
        let (icon, color, text) = match merge.state {
            Mergeability::Clean => (
                IconName::CircleCheck,
                theme.success,
                "This branch has no conflicts with the base branch".into(),
            ),
            Mergeability::Unstable => (
                IconName::CircleAlert,
                theme.warning,
                "Some checks were not successful".into(),
            ),
            Mergeability::Conflicting => (
                IconName::CircleX,
                theme.danger,
                "This branch has conflicts that must be resolved".into(),
            ),
            Mergeability::Blocked => (IconName::CircleX, theme.warning, blocked_reason(detail)),
            Mergeability::Behind => (
                IconName::CircleAlert,
                theme.warning,
                "This branch is out-of-date with the base branch".into(),
            ),
            Mergeability::Draft => (
                IconName::CircleDot,
                theme.muted_foreground,
                "This pull request is still a draft".into(),
            ),
            Mergeability::Unknown if self.checking_gave_up => (
                IconName::CircleDot,
                theme.muted_foreground,
                "GitHub has not worked out whether it can be merged yet".into(),
            ),
            Mergeability::Unknown => (
                IconName::CircleDot,
                theme.muted_foreground,
                "Checking whether it can be merged…".into(),
            ),
        };
        let hint = match merge.state {
            Mergeability::Conflicting => {
                Some("Resolve them on GitHub, or merge the base into the branch and push.")
            }
            Mergeability::Behind => {
                Some("Update the branch on GitHub, or merge the base into it and push.")
            }
            Mergeability::Blocked => {
                Some("Someone allowed to bypass the rules can merge it on GitHub.")
            }
            Mergeability::Draft => Some("Mark it ready for review on GitHub to merge it."),
            _ => None,
        };
        let action: AnyElement = match self.merge_method() {
            _ if !merge.can_merge => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Only those with write access can merge.")
                .into_any_element(),
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("The repository allows no way to merge.")
                .into_any_element(),
            Some(method) => {
                let enabled = merge.state.allows_merge() && self.busy.is_none();
                let label = match self.busy {
                    Some(MERGING) => MERGING,
                    _ => method.verb(),
                };
                let methods = merge.methods.clone();
                let owner = cx.entity().downgrade();
                DropdownButton::new("pull-merge")
                    .small()
                    .button(
                        Button::new("pull-merge-button")
                            .label(label)
                            .when(enabled, |button| button.success())
                            .disabled(!enabled)
                            .on_click(
                                cx.listener(|view, _, window, cx| view.confirm_merge(window, cx)),
                            ),
                    )
                    .dropdown_menu(move |menu, _, _| {
                        methods.iter().fold(menu, |menu, &choice| {
                            let owner = owner.clone();
                            menu.item(
                                PopupMenuItem::new(choice.label())
                                    .checked(choice == method)
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |view, cx| {
                                            view.method = Some(choice);
                                            cx.notify();
                                        });
                                    }),
                            )
                        })
                    })
                    .into_any_element()
            }
        };
        Some(
            h_flex()
                .items_start()
                .gap_3()
                .child(
                    div().w(gpui_kit::px(32.)).flex_none().pt_3().child(
                        gpui_kit::component::Icon::new(IconName::GitMerge)
                            .small()
                            .text_color(color),
                    ),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .child(
                            v_flex()
                                .px_3()
                                .py_2()
                                .gap_0p5()
                                .border_b_1()
                                .border_color(theme.border)
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_sm()
                                        .font_medium()
                                        .child(
                                            gpui_kit::component::Icon::new(icon)
                                                .small()
                                                .text_color(color),
                                        )
                                        .child(div().min_w_0().child(text)),
                                )
                                .children(hint.map(|hint| {
                                    div()
                                        .pl_6()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(hint)
                                })),
                        )
                        .child(
                            h_flex()
                                .px_3()
                                .py_2()
                                .gap_2()
                                .child(div().harness_mark(|| "pull-merge").child(action))
                                // the view stopped asking: the next is the user's
                                .when(
                                    merge.state == Mergeability::Unknown && self.checking_gave_up,
                                    |this| {
                                        this.child(
                                            Button::new("pull-check-again")
                                                .small()
                                                .ghost()
                                                .icon(IconName::RefreshCw)
                                                .label("Check again")
                                                .on_click(cx.listener(|_, _, _, cx| {
                                                    cx.emit(OverviewEvent::CheckAgain)
                                                })),
                                        )
                                    },
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The comment box at the conversation's end, as the viewer, with the
    /// last request's refusal above it.
    pub(super) fn comment_box(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let viewer = &self.detail.viewer;
        let empty = self.comment.read(cx).value().trim().is_empty();
        let label = match self.busy {
            Some(COMMENTING) => COMMENTING,
            _ => "Comment",
        };
        v_flex()
            .gap_2()
            .children(self.error.clone().map(|error| {
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.danger)
                    .text_sm()
                    .child(
                        gpui_kit::component::Icon::new(IconName::CircleX)
                            .small()
                            .text_color(theme.danger),
                    )
                    .child(div().flex_1().min_w_0().child(error))
            }))
            .child(
                h_flex()
                    .items_start()
                    .gap_3()
                    .child(crate::graph::avatar(
                        &viewer.login,
                        viewer.avatar.clone().map(SharedString::from),
                        gpui_kit::px(32.),
                        theme.border,
                        false,
                        cx,
                    ))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(div().text_sm().font_semibold().child(viewer.login.clone()))
                            .child(
                                div()
                                    .harness_mark(|| "pull-comment-field")
                                    .child(Textarea::new(&self.comment)),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("Markdown · ⌘↩ to comment"),
                                    )
                                    .child(
                                        div().harness_mark(|| "pull-comment").child(
                                            Button::new("pull-comment")
                                                .small()
                                                .label(label)
                                                .when(!empty, |button| button.primary())
                                                .disabled(empty || self.busy.is_some())
                                                .on_click(cx.listener(|view, _, window, cx| {
                                                    view.submit_comment(window, cx)
                                                })),
                                        ),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// "Submit a review", atop the side column of an open pull request; the
/// composer sends it.
pub(super) fn review_button(
    detail: &PullDetail,
    composer: &Entity<PullComposer>,
    cx: &App,
) -> Option<AnyElement> {
    if !matches!(detail.state, PullState::Open | PullState::Draft) {
        return None;
    }
    let busy = composer.read(cx).busy;
    let label = match busy {
        Some(REVIEWING) => REVIEWING,
        _ => "Submit a review",
    };
    let composer = composer.clone();
    Some(
        div()
            .harness_mark(|| "pull-review")
            .child(
                Button::new("pull-review")
                    .small()
                    .w_full()
                    .success()
                    .label(label)
                    .disabled(busy.is_some())
                    .on_click(move |_, window, cx| {
                        composer.update(cx, |composer, cx| composer.open_review(window, cx))
                    }),
            )
            .into_any_element(),
    )
}

/// Why branch protection holds the merge, as far as the page shows.
fn blocked_reason(detail: &PullDetail) -> SharedString {
    match detail.review {
        Review::ChangesRequested => "Changes were requested: merging is blocked".into(),
        Review::Required => "Review required: merging is blocked".into(),
        _ if detail
            .checks
            .iter()
            .any(|check| check.state != Checks::Passing) =>
        {
            "Required checks have not passed: merging is blocked".into()
        }
        _ => "Merging is blocked by the branch's rules".into(),
    }
}
