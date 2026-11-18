//! Review comments on the lines of Files changed, as GitHub's page has them:
//! the + beside a line (or the end of the lines dragged over) opens a box
//! under it, which posts a single comment, starts a review, or adds to the
//! review in progress; a thread takes replies and is resolved or opened
//! again. The boxes are the tab's (they need a window to be made); what they
//! send goes through the composer, one request at a time, and a box goes once
//! GitHub has its text.

use gpui_kit::component::diff::{DiffLineRange, DiffSide};
use gpui_kit::component::input::Textarea;

use super::*;

/// A line comment being written, under the last of its lines.
pub(super) struct Draft {
    pub(super) id: u64,
    pub(super) range: DiffLineRange,
    /// The head whose diff the lines are of: a push since moves them.
    pub(super) head: String,
    pub(super) editor: Entity<TextareaState>,
    _events: gpui_kit::Subscription,
}

/// A reply being written under a thread.
pub(super) struct Reply {
    pub(super) editor: Entity<TextareaState>,
    _events: gpui_kit::Subscription,
}

/// How long after a press cleared the lines dragged over their + may still
/// take them (see [`DraggedLines`]).
const DRAGGED_LINES_KEPT: std::time::Duration = std::time::Duration::from_secs(1);

/// The lines last dragged over, kept past the press that clears them.
///
/// gpui-kit's + at the end of a selection is drawn with the selected range,
/// but a press anywhere clears the window's text selection, and the diff's
/// line selection with it, before the button's click runs: the button drawn
/// again by then has its line alone. So the range is noted as the drag ends,
/// and a + on its last line just after takes the whole range.
#[derive(Default)]
pub(super) struct DraggedLines {
    range: Option<DiffLineRange>,
    cleared: Option<std::time::Instant>,
}

impl DraggedLines {
    pub(super) fn note(&mut self, event: &gpui_kit::component::diff::DiffEvent) {
        use gpui_kit::component::diff::DiffEvent;
        match event {
            DiffEvent::SelectionEnded(range) => {
                self.range = Some(range.clone());
                self.cleared = None;
            }
            DiffEvent::SelectionChanged(None) => {
                self.cleared.get_or_insert_with(std::time::Instant::now);
            }
            _ => {}
        }
    }

    /// `range`, or the lines dragged over when `range` is their last line
    /// alone and they were cleared only just now (by the + press itself).
    pub(super) fn widen(&mut self, range: DiffLineRange) -> DiffLineRange {
        let recent = self
            .cleared
            .is_none_or(|cleared| cleared.elapsed() < DRAGGED_LINES_KEPT);
        let alone = range.start() == range.end() && range.side() == range.end_side();
        match self.range.take() {
            Some(dragged)
                if recent
                    && alone
                    && dragged.path() == range.path()
                    && dragged.end_side() == range.end_side()
                    && dragged.end() == range.end() =>
            {
                dragged
            }
            _ => range,
        }
    }
}

/// The lines a box comments on, as GitHub's inputs take them: the thread
/// hangs under the last, and a range says where it starts.
pub(super) fn thread_lines(range: &DiffLineRange) -> gh::ThreadLines {
    let side = |side: DiffSide| match side {
        DiffSide::Original => Side::Left,
        _ => Side::Right,
    };
    let (mut from, mut to) = (
        (range.side(), range.start()),
        (range.end_side(), range.end()),
    );
    // dragged upwards: GitHub wants the start first
    if from.0 == to.0 && from.1 > to.1 {
        std::mem::swap(&mut from, &mut to);
    }
    gh::ThreadLines {
        path: range.path().to_string(),
        side: side(to.0),
        line: to.1 as u64,
        start: (from != to).then(|| (side(from.0), from.1 as u64)),
    }
}

fn sign(side: Side) -> &'static str {
    match side {
        Side::Left => "−",
        Side::Right => "+",
    }
}

/// "Lines +5 to +7": a thread's or a box's lines, the removed side's marked −.
pub(super) fn range_label(start: (Side, u64), end: (Side, u64)) -> String {
    format!(
        "Lines {}{} to {}{}",
        sign(start.0),
        start.1,
        sign(end.0),
        end.1
    )
}

/// "Line +7", "Lines +5 to +7", as the box's heading says.
fn lines_label(lines: &gh::ThreadLines) -> String {
    match lines.start {
        None => format!("Line {}{}", sign(lines.side), lines.line),
        Some(start) => range_label(start, (lines.side, lines.line)),
    }
}

impl RepoView {
    /// The open pull request's composer, which sends what its view writes.
    fn pull_composer(&self) -> Option<Entity<PullComposer>> {
        self.center
            .pull()
            .and_then(|pull| pull.overview.as_ref())
            .map(|overview| overview.composer.clone())
    }

    /// Whether the open pull request takes review comments: open, and its
    /// detail (with the token to write with) in.
    pub(super) fn pull_takes_comments(&self) -> bool {
        self.center.pull().is_some_and(|pull| {
            pull.overview.is_some()
                && matches!(
                    &pull.detail,
                    Load::Ready(detail) if matches!(detail.state, PullState::Open | PullState::Draft)
                )
        })
    }

    /// Opens a box for a comment on `range` (the + of a line, or of the lines
    /// dragged over), or brings back the one already there.
    pub(super) fn add_line_comment(
        &mut self,
        range: DiffLineRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.pull_takes_comments() {
            return;
        }
        let Some(pull) = self.center.pull_mut() else {
            return;
        };
        let range = pull.selection.widen(range);
        // the lines are of the diff on screen
        let Some(head) = pull.diff_head.clone() else {
            return;
        };
        if let Some(draft) = pull.drafts.iter().find(|draft| draft.range == range) {
            let editor = draft.editor.clone();
            editor.update(cx, |state, cx| state.focus(window, cx));
            return;
        }
        let id = pull.next_draft;
        pull.next_draft += 1;
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("Leave a comment")
        });
        // ⌘↩ (Ctrl+Enter elsewhere) posts it as GitHub's does: into the
        // review in progress, else a single comment
        let _events = cx.subscribe_in(
            &editor,
            window,
            move |view: &mut RepoView, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter {
                    secondary: true, ..
                } = event
                {
                    view.post_line_comment(id, None, window, cx)
                }
            },
        );
        pull.drafts.push(Draft {
            id,
            range,
            head,
            editor: editor.clone(),
            _events,
        });
        // the box takes the keys once it is drawn
        window.defer(cx, move |window, cx| {
            editor.update(cx, |state, cx| state.focus(window, cx));
        });
        cx.notify();
    }

    /// Sends box `id` the way `target` says, or GitHub's way for ⌘↩.
    pub(super) fn post_line_comment(
        &mut self,
        id: u64,
        target: Option<gh::ThreadTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(composer), Some(pull)) = (self.pull_composer(), self.center.pull()) else {
            return;
        };
        let Some(draft) = pull.drafts.iter().find(|draft| draft.id == id) else {
            return;
        };
        let body = draft.editor.read(cx).value().trim().to_owned();
        let lines = thread_lines(&draft.range);
        let head = draft.head.clone();
        composer.update(cx, |composer, cx| {
            let target = target.unwrap_or_else(|| match &composer.detail.pending_review {
                Some(review) => gh::ThreadTarget::Pending(review.id.clone()),
                None => gh::ThreadTarget::Single,
            });
            let comment = gh::LineComment {
                lines,
                body,
                target,
            };
            composer.post_line_comment(id, head, comment, window, cx)
        });
    }

    pub(super) fn cancel_line_comment(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(pull) = self.center.pull_mut() {
            pull.drafts.retain(|draft| draft.id != id);
            cx.notify();
        }
    }

    /// Opens a reply box under thread `thread`.
    pub(super) fn open_reply(
        &mut self,
        thread: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pull) = self.center.pull_mut() else {
            return;
        };
        if let Some(reply) = pull.replies.get(&thread) {
            let editor = reply.editor.clone();
            editor.update(cx, |state, cx| state.focus(window, cx));
            return;
        }
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .placeholder("Reply…")
        });
        let id = thread.clone();
        let _events = cx.subscribe_in(
            &editor,
            window,
            move |view: &mut RepoView, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter {
                    secondary: true, ..
                } = event
                {
                    view.post_reply(id.clone(), window, cx)
                }
            },
        );
        pull.replies.insert(
            thread,
            Reply {
                editor: editor.clone(),
                _events,
            },
        );
        window.defer(cx, move |window, cx| {
            editor.update(cx, |state, cx| state.focus(window, cx));
        });
        cx.notify();
    }

    pub(super) fn post_reply(
        &mut self,
        thread: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(composer), Some(pull)) = (self.pull_composer(), self.center.pull()) else {
            return;
        };
        let Some(reply) = pull.replies.get(&thread) else {
            return;
        };
        let body = reply.editor.read(cx).value().trim().to_owned();
        composer.update(cx, |composer, cx| {
            composer.post_reply(thread, body, window, cx)
        });
    }

    pub(super) fn cancel_reply(&mut self, thread: &str, cx: &mut Context<Self>) {
        if let Some(pull) = self.center.pull_mut() {
            pull.replies.remove(thread);
            cx.notify();
        }
    }

    pub(super) fn resolve_thread(
        &mut self,
        thread: String,
        resolved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(composer) = self.pull_composer() {
            composer.update(cx, |composer, cx| {
                composer.set_resolved(thread, resolved, window, cx)
            });
        }
    }

    /// "Finish your review" from Files changed: the composer's dialog.
    pub(super) fn finish_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(composer) = self.pull_composer() {
            composer.update(cx, |composer, cx| composer.open_review(window, cx));
        }
    }

    /// GitHub took a line comment or a reply: its box goes, unless more was
    /// typed in it meanwhile.
    pub(super) fn line_comment_posted(&mut self, sent: &Sent, cx: &mut Context<Self>) {
        let Some(pull) = self.center.pull_mut() else {
            return;
        };
        match sent {
            Sent::Draft(id, body) => pull.drafts.retain(|draft| {
                draft.id != *id || draft.editor.read(cx).value().trim() != body.as_str()
            }),
            Sent::Reply(thread, body) => {
                if pull
                    .replies
                    .get(thread)
                    .is_some_and(|reply| reply.editor.read(cx).value().trim() == body.as_str())
                {
                    pull.replies.remove(thread);
                }
            }
            Sent::Comment(_) => {}
        }
        cx.notify();
    }
}

/// What the cards under the lines need of the tab, copied for the diff's
/// renderer.
#[derive(Clone)]
pub(super) struct ReviewCards {
    pub(super) owner: gpui_kit::WeakEntity<RepoView>,
    /// Open, with the token to write with.
    pub(super) writable: bool,
    /// A request is on its way: the buttons wait.
    pub(super) busy: bool,
    /// The viewer's review in progress, which new comments go into.
    pub(super) pending: bool,
    pub(super) drafts: Rc<Vec<(u64, gh::ThreadLines, Entity<TextareaState>)>>,
    pub(super) replies: Rc<HashMap<String, Entity<TextareaState>>>,
}

impl ReviewCards {
    /// The box of draft `id`, under its last line.
    pub(super) fn draft_card(&self, id: u64, cx: &App) -> AnyElement {
        let Some((_, lines, editor)) = self.drafts.iter().find(|(draft, _, _)| *draft == id) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let empty = editor.read(cx).value().trim().is_empty();
        let ready = !empty && !self.busy;
        let owner = self.owner.clone();
        let send = move |target: Option<gh::ThreadTarget>| {
            let owner = owner.clone();
            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                let _ = owner.update(cx, |view, cx| {
                    view.post_line_comment(id, target.clone(), window, cx)
                });
            }
        };
        let cancel = {
            let owner = self.owner.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = owner.update(cx, |view, cx| view.cancel_line_comment(id, cx));
            }
        };
        let buttons = h_flex()
            .gap_2()
            .child(div().flex_1())
            .child(
                Button::new(("draft-cancel", id))
                    .small()
                    .outline()
                    .label("Cancel")
                    .on_click(cancel),
            )
            .map(|row| {
                if self.pending {
                    row.child(
                        div().harness_mark(|| "draft-add").child(
                            Button::new(("draft-add", id))
                                .small()
                                .when(ready, |button| button.primary())
                                .disabled(!ready)
                                .label("Add review comment")
                                .on_click(send(None)),
                        ),
                    )
                } else {
                    row.child(
                        div().harness_mark(|| "draft-single").child(
                            Button::new(("draft-single", id))
                                .small()
                                .outline()
                                .disabled(!ready)
                                .label("Add single comment")
                                .on_click(send(Some(gh::ThreadTarget::Single))),
                        ),
                    )
                    .child(
                        div().harness_mark(|| "draft-start").child(
                            Button::new(("draft-start", id))
                                .small()
                                .when(ready, |button| button.primary())
                                .disabled(!ready)
                                .label("Start a review")
                                .on_click(send(Some(gh::ThreadTarget::StartReview))),
                        ),
                    )
                }
            });
        v_flex()
            .mx_3()
            .my_1()
            .p_2()
            .gap_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.ring)
            .bg(theme.background)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("Comment on {}", lines_label(lines).to_lowercase())),
            )
            .child(
                div()
                    .harness_mark(|| "draft-field")
                    .on_action({
                        // Esc closes the box while it is empty; with text it
                        // keeps it, as a slip would lose what is written
                        let (owner, editor) = (self.owner.clone(), editor.clone());
                        move |_: &gpui_kit::component::input::Escape, _, cx| {
                            if editor.read(cx).value().trim().is_empty() {
                                let _ =
                                    owner.update(cx, |view, cx| view.cancel_line_comment(id, cx));
                            }
                        }
                    })
                    .child(Textarea::new(editor).aria_label("Line comment")),
            )
            .child(buttons)
            .into_any_element()
    }

    /// The reply box, or the Reply button, and Resolve, under thread
    /// `thread`'s comments.
    pub(super) fn thread_actions(&self, thread: &gh::ReviewThread, cx: &App) -> Option<AnyElement> {
        if !self.writable || thread.id.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let id = thread.id.clone();
        let resolve = (thread.resolved && thread.can_unresolve)
            .then_some(("Unresolve conversation", false))
            .or((!thread.resolved && thread.can_resolve).then_some(("Resolve conversation", true)));
        let resolve_button = resolve.map(|(label, resolved)| {
            let owner = self.owner.clone();
            let id = id.clone();
            div().harness_mark(|| "thread-resolve").child(
                Button::new(SharedString::from(format!("resolve-{}", thread.id)))
                    .small()
                    .outline()
                    .label(label)
                    .disabled(self.busy)
                    .on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            view.resolve_thread(id.clone(), resolved, window, cx)
                        });
                    }),
            )
        });
        let row = match self.replies.get(&thread.id) {
            Some(editor) => {
                let empty = editor.read(cx).value().trim().is_empty();
                let (post, cancel) = (self.owner.clone(), self.owner.clone());
                let (post_id, cancel_id) = (id.clone(), id.clone());
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .harness_mark(|| "reply-field")
                            .on_action({
                                // as a line comment's box: Esc closes it empty
                                let (owner, editor, id) =
                                    (self.owner.clone(), editor.clone(), id.clone());
                                move |_: &gpui_kit::component::input::Escape, _, cx| {
                                    if editor.read(cx).value().trim().is_empty() {
                                        let _ =
                                            owner.update(cx, |view, cx| view.cancel_reply(&id, cx));
                                    }
                                }
                            })
                            .child(Textarea::new(editor).aria_label("Reply")),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .children(resolve_button)
                            .child(div().flex_1())
                            .child(
                                Button::new(SharedString::from(format!("reply-cancel-{id}")))
                                    .small()
                                    .outline()
                                    .label("Cancel")
                                    .on_click(move |_, _, cx| {
                                        let _ = cancel.update(cx, |view, cx| {
                                            view.cancel_reply(&cancel_id, cx)
                                        });
                                    }),
                            )
                            .child(
                                div().harness_mark(|| "reply-send").child(
                                    Button::new(SharedString::from(format!("reply-send-{id}")))
                                        .small()
                                        .when(!empty && !self.busy, |button| button.primary())
                                        .disabled(empty || self.busy)
                                        .label(if self.pending {
                                            "Add review comment"
                                        } else {
                                            "Reply"
                                        })
                                        .on_click(move |_, window, cx| {
                                            let _ = post.update(cx, |view, cx| {
                                                view.post_reply(post_id.clone(), window, cx)
                                            });
                                        }),
                                ),
                            ),
                    )
                    .into_any_element()
            }
            None => {
                let owner = self.owner.clone();
                h_flex()
                    .gap_2()
                    .when(thread.can_reply, |this| {
                        this.child(
                            div().harness_mark(|| "thread-reply").child(
                                Button::new(SharedString::from(format!("reply-{id}")))
                                    .small()
                                    .ghost()
                                    .icon(IconName::MessageSquare)
                                    .label("Reply…")
                                    .on_click(move |_, window, cx| {
                                        let _ = owner.update(cx, |view, cx| {
                                            view.open_reply(id.clone(), window, cx)
                                        });
                                    }),
                            ),
                        )
                    })
                    .child(div().flex_1())
                    .children(resolve_button)
                    .into_any_element()
            }
        };
        Some(
            div()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(theme.border)
                .child(row)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_the_lines_github_takes() {
        let one = DiffLineRange::new("a.rs", DiffSide::Modified, 7, 7);
        assert_eq!(
            thread_lines(&one),
            gh::ThreadLines {
                path: "a.rs".into(),
                side: Side::Right,
                line: 7,
                start: None
            }
        );
        // dragged upwards: the last line is still the one it hangs under
        let up = DiffLineRange::new("a.rs", DiffSide::Modified, 9, 5);
        let lines = thread_lines(&up);
        assert_eq!((lines.line, lines.start), (9, Some((Side::Right, 5))));
        assert_eq!(lines_label(&lines), "Lines +5 to +9");
        // from a removed line to an added one
        let across =
            DiffLineRange::new("a.rs", DiffSide::Original, 3, 4).with_end_side(DiffSide::Modified);
        let lines = thread_lines(&across);
        assert_eq!(lines.side, Side::Right);
        assert_eq!(lines.start, Some((Side::Left, 3)));
        assert_eq!(lines_label(&one_line()), "Line −2");
    }

    #[test]
    fn the_plus_at_the_end_of_dragged_lines_takes_them_all() {
        use gpui_kit::component::diff::DiffEvent;
        let dragged = DiffLineRange::new("a.rs", DiffSide::Modified, 4, 6);
        let line = |n| DiffLineRange::new("a.rs", DiffSide::Modified, n, n);
        let mut lines = DraggedLines::default();
        lines.note(&DiffEvent::SelectionEnded(dragged.clone()));
        // the + press clears the selection, then the click gives line 6 alone
        lines.note(&DiffEvent::SelectionChanged(None));
        assert_eq!(lines.widen(line(6)), dragged);
        // taken once: a later + on line 6 is line 6
        assert_eq!(lines.widen(line(6)), line(6));
        // a + on another line is that line
        lines.note(&DiffEvent::SelectionEnded(dragged.clone()));
        assert_eq!(lines.widen(line(5)), line(5));
        // cleared long ago (another press): the line alone
        let mut old = DraggedLines {
            range: Some(dragged),
            cleared: Some(std::time::Instant::now() - 2 * DRAGGED_LINES_KEPT),
        };
        assert_eq!(old.widen(line(6)), line(6));
    }

    fn one_line() -> gh::ThreadLines {
        gh::ThreadLines {
            path: "a.rs".into(),
            side: Side::Left,
            line: 2,
            start: None,
        }
    }
}
