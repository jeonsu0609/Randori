//! A pull request on its own, as its GitHub page shows it, in the place of the
//! revisions and the diff (as GitKraken does): an Overview tab with the
//! description, the conversation and the reviewers, labels and checks beside
//! them, and a Files changed tab with the pull request's diff and its review
//! threads under the lines they are on.
//!
//! The diff is computed from the repository when both commits are in it (the
//! merge base of the base and the head, to the head, as GitHub does); a pull
//! request from a fork, or one not fetched yet, takes GitHub's diff instead.

use gpui_kit::component::diff::{DiffAnnotation, DiffFileStatus, DiffLinePosition, DiffSide};
use gpui_kit::component::text::TextView;

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::{EventEmitter, Hsla};

use super::github_access::{github_token, within};
use super::*;
use crate::github;
use crate::github::pull::{
    self as gh, Person, PullDetail, PullState, ReviewState, Side, TimelineItem,
};
use crate::github::{Checks, FetchError, Review};
use futures::FutureExt as _;

/// Checks listed before the rest are counted only.
const CHECKS_SHOWN: usize = 6;

/// Key context of the pull request view; Esc closes it.
pub(super) const PULL_CONTEXT: &str = "RandoriPull";

#[derive(Clone, Copy, PartialEq)]
pub(super) enum PullTab {
    Overview,
    Files,
}

/// Where the diff came from, said above it.
#[derive(Clone, Copy, PartialEq)]
enum DiffSource {
    Repository,
    GitHub,
}

/// One changed file of the pull request.
struct PullFile {
    path: SharedString,
    name: SharedString,
    dir: SharedString,
    status: DiffFileStatus,
    additions: usize,
    deletions: usize,
}

/// Where the review threads are, worked out once per detail rather than on
/// every frame of the Files changed tab.
#[derive(Default)]
struct ThreadIndex {
    /// Each thread under its line; outdated threads have none.
    annotations: Rc<Vec<DiffAnnotation>>,
    per_file: Rc<HashMap<SharedString, usize>>,
    outdated: usize,
}

impl ThreadIndex {
    fn of(threads: &[gh::ReviewThread]) -> Self {
        let mut per_file: HashMap<SharedString, usize> = HashMap::new();
        for thread in threads {
            *per_file
                .entry(SharedString::from(thread.path.clone()))
                .or_default() += 1;
        }
        let annotations = threads
            .iter()
            .enumerate()
            .filter_map(|(ix, thread)| {
                let line = thread.line? as usize;
                let side = match thread.side {
                    Side::Left => DiffSide::Original,
                    Side::Right => DiffSide::Modified,
                };
                Some(DiffAnnotation::line(
                    ("thread", ix),
                    DiffLinePosition::new(thread.path.clone(), side, line),
                ))
            })
            .collect();
        Self {
            annotations: Rc::new(annotations),
            per_file: Rc::new(per_file),
            outdated: threads
                .iter()
                .filter(|thread| thread.line.is_none())
                .count(),
        }
    }
}

pub(super) struct PullView {
    number: u64,
    /// From the list, shown while the detail loads.
    title: SharedString,
    url: String,
    /// The commits the diff runs between, from the list, so the diff starts
    /// at once instead of after the detail.
    base_oid: String,
    head_oid: String,
    /// The base branch: retargeting the pull request changes its diff.
    base_ref: String,
    /// GitHub's last update of the pull request; a newer one in the list
    /// reloads the view.
    updated_at: String,
    tab: PullTab,
    pub(super) handle: FocusHandle,
    detail: Load<Rc<PullDetail>>,
    threads: ThreadIndex,
    /// The Overview, its own (cached) view: the tab's other redraws leave
    /// a long conversation alone.
    overview: Option<Entity<PullOverview>>,
    /// Dropped with the view, so closed pull requests leave nothing behind.
    _overview_events: Option<gpui_kit::Subscription>,
    files: Load<Rc<Vec<PullFile>>>,
    /// The changed-file list, virtualized: a large pull request has
    /// thousands of files.
    files_scroll: UniformListScrollHandle,
    source: Option<DiffSource>,
    selected_file: Option<usize>,
    pub(super) diff: Entity<DiffState>,
    /// Bumped by each load, so only the latest answer is shown when a reload
    /// overtakes an earlier one.
    detail_load: u64,
    diff_load: u64,
    /// The loads in flight, dropped with the view or by the next load: closing
    /// the pull request, opening another or reloading cancels them, GitHub's
    /// requests included. A diff thread already computing runs to its end,
    /// and its answer is dropped.
    _detail_task: Option<Task<()>>,
    _diff_task: Option<Task<()>>,
}

/// The Overview tab: the description, the conversation and the facts beside.
pub(super) struct PullOverview {
    detail: Rc<PullDetail>,
    _theme: gpui_kit::Subscription,
}

pub(super) enum OverviewEvent {
    /// "Show in the graph".
    ShowHead,
}

impl EventEmitter<OverviewEvent> for PullOverview {}

impl Render for PullOverview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let detail = self.detail.clone();
        overview(&detail, cx)
    }
}

impl RepoView {
    /// Opens pull request `number` of the section in the pull request view
    /// (by number: the list may have refreshed since it was drawn).
    pub(super) fn open_pull(&mut self, number: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pr) = self.pull_request_numbered(number) else {
            return;
        };
        if let Some(pull) = self.center.pull().filter(|pull| pull.number == pr.number) {
            pull.handle.focus(window, cx);
            return;
        }
        let mode = self.diff_mode;
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(mode));
        let handle = cx.focus_handle();
        handle.focus(window, cx);
        let view = PullView {
            number: pr.number,
            title: pr.title.clone().into(),
            url: pr.url.clone(),
            base_oid: pr.base_oid.clone(),
            base_ref: pr.base_ref.clone(),
            head_oid: pr.head_oid.clone(),
            updated_at: pr.updated_at.clone(),
            tab: PullTab::Overview,
            handle,
            detail: Load::Loading,
            threads: ThreadIndex::default(),
            overview: None,
            _overview_events: None,
            files: Load::Loading,
            files_scroll: UniformListScrollHandle::new(),
            source: None,
            selected_file: None,
            diff,
            detail_load: 0,
            diff_load: 0,
            _detail_task: None,
            _diff_task: None,
        };
        self.center = Center::Pull(Box::new(view));
        // the two load side by side: the diff needs only the list's commits
        self.load_pull(true, true, cx);
        cx.notify();
    }

    pub(super) fn close_pull(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.center.pull().is_some() {
            self.center = Center::Revisions;
            self.log_focus.focus(window, cx);
            cx.notify();
        }
    }

    /// After the list refreshed: reloads the open pull request when GitHub
    /// updated it, and its diff too when its commits moved.
    pub(super) fn refresh_open_pull(&mut self, cx: &mut Context<Self>) {
        let Some(number) = self.center.pull().map(|pull| pull.number) else {
            return;
        };
        let Some(pr) = self.pull_request_numbered(number) else {
            return;
        };
        let (base, head, base_ref, updated) = (
            pr.base_oid.clone(),
            pr.head_oid.clone(),
            pr.base_ref.clone(),
            pr.updated_at.clone(),
        );
        let Some(pull) = self.center.pull_mut() else {
            return;
        };
        if pull.updated_at == updated {
            return;
        }
        // new commits on the base branch leave the merge base, and so the
        // diff, as it was: only a push to the head or a new base branch
        // changes it
        let moved = pull.head_oid != head || pull.base_ref != base_ref;
        pull.base_ref = base_ref;
        pull.updated_at = updated;
        pull.base_oid = base;
        pull.head_oid = head;
        self.load_pull(true, moved, cx);
    }

    /// Loads the detail (the Overview) and the diff, either or both, side by
    /// side. They share one token lookup. The diff is computed from a load of
    /// the repository of its own, so a large pull request does not hold up
    /// the tab's worker; GitHub's requests and their parsing run in the
    /// background. A reload keeps what is shown until its answer arrives.
    fn load_pull(&mut self, detail: bool, diff: bool, cx: &mut Context<Self>) {
        let root = self.workspace.as_ref().map(|ws| ws.root.clone());
        let access = self.github_access();
        let Some(pull) = self.center.pull_mut() else {
            return;
        };
        let number = pull.number;
        let (base, head) = (pull.base_oid.clone(), pull.head_oid.clone());
        let detail_load = detail.then(|| {
            pull.detail_load += 1;
            pull.detail_load
        });
        let diff_load = diff.then(|| {
            pull.diff_load += 1;
            pull.diff_load
        });
        let client = cx.http_client();
        let executor = cx.background_executor().clone();
        let token = access.as_ref().map(|(repo, cached)| {
            github_token(repo.host.clone(), cached.clone(), executor).shared()
        });

        let detail_task = detail_load.map(|load| {
            let (access, token, client) = (access.clone(), token.clone(), client.clone());
            cx.spawn(async move |this, cx| {
                let result = match (access, token) {
                    (Some((repo, _)), Some(token)) => match token.await {
                        Some(token) => {
                            let executor = cx.background_executor().clone();
                            let fetched = cx
                                .background_spawn(within(
                                    executor,
                                    gh::fetch_detail(client, repo, token.clone(), number),
                                ))
                                .await;
                            Ok((token, fetched))
                        }
                        None => Err("Sign in with `gh auth login` to see it.".to_owned()),
                    },
                    _ => Err("GitHub is turned off in Settings.".to_owned()),
                };
                let _ = this.update(cx, |view, cx| {
                    view.show_pull_detail(number, load, result, cx)
                });
            })
        });

        let diff_task = diff_load.map(|load| {
            cx.spawn(async move |this, cx| {
                let local = match root {
                    Some(root) => {
                        crate::worker::on_own_thread("randori-pull-diff", move || {
                            gg_lib::worker::commit_range_diff_apart(&root, &base, &head)
                                .map(|changes| changes.map(|changes| diff_files(&changes)))
                        })
                        .await
                    }
                    None => Ok(None),
                };
                let (files, source) = match local {
                    Ok(Some(files)) => (Ok(files), DiffSource::Repository),
                    other => {
                        if let Err(err) = other {
                            log::warn!("pull request diff from the repository: {err:#}");
                        }
                        let diff = github_diff(access, token, client, number, cx).await;
                        (diff, DiffSource::GitHub)
                    }
                };
                let _ = this.update(cx, |view, cx| {
                    view.show_pull_files(number, load, files, source, cx)
                });
            })
        });

        // replacing a load in flight cancels it
        if let Some(pull) = self.center.pull_mut() {
            if detail_task.is_some() {
                pull._detail_task = detail_task;
            }
            if diff_task.is_some() {
                pull._diff_task = diff_task;
            }
        }
    }

    fn show_pull_detail(
        &mut self,
        number: u64,
        load: u64,
        result: Result<(String, Result<PullDetail, FetchError>), String>,
        cx: &mut Context<Self>,
    ) {
        let detail = match result {
            Ok((token, detail)) => {
                self.remember_github_token(token);
                detail.map_err(fetch_message)
            }
            Err(message) => Err(message),
        };
        let Some(pull) = self
            .center
            .pull_mut()
            .filter(|pull| pull.number == number && pull.detail_load == load)
        else {
            return;
        };
        match detail {
            Ok(detail) => {
                let detail = Rc::new(detail);
                pull.title = detail.title.clone().into();
                pull.threads = ThreadIndex::of(&detail.threads);
                pull.detail = Load::Ready(detail.clone());
                match &pull.overview {
                    Some(overview) => overview.update(cx, |overview, cx| {
                        overview.detail = detail;
                        cx.notify();
                    }),
                    None => {
                        let overview = cx.new(|cx| PullOverview {
                            detail,
                            _theme: cx
                                .observe_global::<gpui_kit::component::Theme>(|_, cx| cx.notify()),
                        });
                        let subscription =
                            cx.subscribe(&overview, |view, _, event, cx| match event {
                                OverviewEvent::ShowHead => view.show_pull_head_now(cx),
                            });
                        if let Some(pull) = self.center.pull_mut() {
                            pull.overview = Some(overview);
                            pull._overview_events = Some(subscription);
                        }
                    }
                }
            }
            Err(message) => self.pull_detail_failed(number, &message, cx),
        }
        cx.notify();
    }

    fn pull_detail_failed(&mut self, number: u64, message: &str, cx: &mut Context<Self>) {
        if let Some(pull) = self.center.pull_mut().filter(|pull| pull.number == number) {
            // a failed reload keeps the detail already shown
            if !matches!(pull.detail, Load::Ready(_)) {
                pull.detail = Load::Failed(message.to_owned().into());
            }
            cx.notify();
        }
    }

    fn show_pull_files(
        &mut self,
        number: u64,
        load: u64,
        files: Result<Vec<DiffFile>, String>,
        source: DiffSource,
        cx: &mut Context<Self>,
    ) {
        let Some(pull) = self
            .center
            .pull_mut()
            .filter(|pull| pull.number == number && pull.diff_load == load)
        else {
            return;
        };
        match files {
            Ok(files) => {
                pull.files = Load::Ready(Rc::new(
                    files
                        .iter()
                        .map(|file| {
                            let path = file.path().clone();
                            let (dir, name) = match path.rsplit_once('/') {
                                Some((dir, name)) => (dir.to_owned(), name.to_owned()),
                                None => (String::new(), path.to_string()),
                            };
                            PullFile {
                                path,
                                name: name.into(),
                                dir: dir.into(),
                                status: file.status(),
                                additions: file.additions(),
                                deletions: file.deletions(),
                            }
                        })
                        .collect(),
                ));
                pull.source = Some(source);
                pull.selected_file = None;
                pull.diff.update(cx, |state, cx| state.set_files(files, cx));
            }
            Err(message) => {
                if !matches!(pull.files, Load::Ready(_)) {
                    pull.files = Load::Failed(message.into());
                }
            }
        }
        cx.notify();
    }

    fn set_pull_tab(&mut self, tab: PullTab, cx: &mut Context<Self>) {
        if let Some(pull) = self.center.pull_mut() {
            pull.tab = tab;
            cx.notify();
        }
    }

    fn select_pull_file(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(pull) = self.center.pull_mut() else {
            return;
        };
        let Load::Ready(files) = &pull.files else {
            return;
        };
        let Some(path) = files.get(ix).map(|file| file.path.clone()) else {
            return;
        };
        pull.selected_file = Some(ix);
        pull.diff
            .update(cx, |state, cx| state.scroll_to_file(path.as_ref(), cx));
        cx.notify();
    }

    /// "Show in the graph" in the Overview, which has no window to hand over:
    /// the tab finishes it on its next frame.
    fn show_pull_head_now(&mut self, cx: &mut Context<Self>) {
        self.show_head_pending = true;
        cx.notify();
    }

    /// The pull request's head in the graph, as clicking it in the list did.
    pub(super) fn show_pull_head(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((oid, number, head)) = self.center.pull().and_then(|pull| match &pull.detail {
            Load::Ready(detail) => Some((
                detail.head_oid.clone(),
                detail.number,
                detail.head_ref.clone(),
            )),
            _ => None,
        }) else {
            return;
        };
        self.center = Center::Revisions;
        self.select_commit(oid, number, head, window, cx);
    }

    // ----- drawing --------------------------------------------------------------------------

    pub(super) fn render_pull(&self, pull: &PullView, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let detail = match &pull.detail {
            Load::Ready(detail) => Some(detail.clone()),
            _ => None,
        };
        let state_tag = detail.as_ref().map(|detail| match detail.state {
            PullState::Open => Tag::success().small().child("Open"),
            PullState::Draft => Tag::secondary().small().child("Draft"),
            PullState::Merged => Tag::primary().small().child("Merged"),
            PullState::Closed => Tag::danger().small().child("Closed"),
        });
        let files_label = match (&pull.files, &detail) {
            (Load::Ready(files), _) => format!("Files changed {}", files.len()),
            (_, Some(detail)) => format!("Files changed {}", detail.changed_files),
            _ => "Files changed".to_owned(),
        };
        let url = pull.url.clone();
        let header = v_flex()
            .flex_none()
            .px_4()
            .pt_3()
            .pb_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        gpui_kit::component::Icon::new(IconName::GitPullRequest)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .child(format!("#{}", pull.number)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_lg()
                            .font_semibold()
                            .child(pull.title.clone()),
                    )
                    .child(
                        Button::new("pull-open-web")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ExternalLink)
                            .tooltip("Open on GitHub")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(
                        Button::new("pull-close")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Back to the revisions (Esc)")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.close_pull(window, cx)),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .children(state_tag)
                    .when_some(detail.clone(), |this, detail| {
                        this.child(crate::graph::avatar(
                            &detail.author.login,
                            detail.author.avatar.clone().map(SharedString::from),
                            gpui_kit::px(18.),
                            theme.border,
                            false,
                            cx,
                        ))
                        .child(div().min_w_0().truncate().child(format!(
                            "{} wants to merge {} into {}",
                            detail.author.login, detail.head_ref, detail.base_ref
                        )))
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.success)
                                .child(format!("+{}", detail.additions)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.danger)
                                .child(format!("−{}", detail.deletions)),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        TabBar::new("pull-tabs")
                            .segmented()
                            .xsmall()
                            .selected_index(usize::from(pull.tab == PullTab::Files))
                            .on_click(cx.listener(|view, ix: &usize, _, cx| {
                                let tab = if *ix == 1 {
                                    PullTab::Files
                                } else {
                                    PullTab::Overview
                                };
                                view.set_pull_tab(tab, cx)
                            }))
                            .child("Overview")
                            .child(files_label),
                    ),
            );

        let content = match pull.tab {
            PullTab::Overview => match &pull.detail {
                Load::Ready(_) => match &pull.overview {
                    Some(overview) => gpui_kit::AnyView::from(overview.clone())
                        .cached(gpui_kit::StyleRefinement::default().size_full())
                        .into_any_element(),
                    None => pull_note(None, cx),
                },
                Load::Loading => pull_note(None, cx),
                Load::Note(message) | Load::Failed(message) => pull_note(Some(message.clone()), cx),
            },
            PullTab::Files => match &pull.files {
                Load::Ready(files) => self.render_pull_files(pull, files, cx),
                Load::Loading => pull_note(None, cx),
                Load::Note(message) | Load::Failed(message) => pull_note(Some(message.clone()), cx),
            },
        };

        v_flex()
            .id("pull-view")
            .key_context(PULL_CONTEXT)
            .track_focus(&pull.handle)
            .size_full()
            .min_h_0()
            .on_action(
                cx.listener(|view, _: &actions::ClosePull, window, cx| view.close_pull(window, cx)),
            )
            .child(header)
            .child(div().flex_1().min_h_0().child(content))
            .into_any_element()
    }
}

/// The Overview's content.
fn overview(detail: &Rc<PullDetail>, cx: &mut Context<PullOverview>) -> AnyElement {
    {
        let theme = cx.theme();
        let mut conversation: Vec<AnyElement> = vec![comment_card(
            pull_id(detail.number, "body", 0),
            &detail.author,
            "opened this pull request",
            &detail.created_at,
            if detail.body.trim().is_empty() {
                "_No description provided._"
            } else {
                &detail.body
            },
            None,
            cx,
        )];
        let earlier = detail
            .timeline_total
            .saturating_sub(detail.timeline.len() as u64);
        if earlier > 0 {
            conversation.push(on_github_row(
                &format!(
                    "{earlier} earlier item{} of the conversation",
                    if earlier == 1 { "" } else { "s" }
                ),
                &detail.url,
                cx,
            ));
        }
        for (ix, item) in detail.timeline.iter().enumerate() {
            conversation.push(match item {
                TimelineItem::Comment { author, body, at } => comment_card(
                    pull_id(detail.number, "comment", ix),
                    author,
                    "commented",
                    at,
                    body,
                    None,
                    cx,
                ),
                TimelineItem::Review {
                    author,
                    state,
                    body,
                    at,
                } => {
                    let (verb, color) = review_words(*state, cx);
                    if body.trim().is_empty() {
                        event_row(author, verb, at, Some(color), cx)
                    } else {
                        comment_card(
                            pull_id(detail.number, "review", ix),
                            author,
                            verb,
                            at,
                            body,
                            Some(color),
                            cx,
                        )
                    }
                }
                TimelineItem::Commit {
                    oid,
                    headline,
                    author,
                    at,
                } => commit_row(oid, headline, author, at, cx),
                TimelineItem::Event { actor, text, at } => event_row(actor, text, at, None, cx),
            });
        }

        let section = |title: &'static str, body: AnyElement| {
            v_flex()
                .gap_1p5()
                .pb_3()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .text_color(theme.muted_foreground)
                        .child(title),
                )
                .child(body)
                .into_any_element()
        };
        let none = |text: &'static str| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let people_row = |person: &Person, note: Option<(SharedString, Hsla)>| {
            h_flex()
                .gap_2()
                .text_sm()
                .child(crate::graph::avatar(
                    &person.login,
                    person.avatar.clone().map(SharedString::from),
                    gpui_kit::px(20.),
                    theme.border,
                    false,
                    cx,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(person.login.clone()),
                )
                .when_some(note, |this, (note, color)| {
                    this.child(div().text_xs().text_color(color).child(note))
                })
                .into_any_element()
        };
        let reviewers = if detail.reviewers.is_empty() {
            none("No reviewers")
        } else {
            v_flex()
                .gap_1p5()
                .children(detail.reviewers.iter().map(|reviewer| {
                    let note = match reviewer.state {
                        None => ("Requested".into(), theme.muted_foreground),
                        Some(state) => {
                            let (_, color) = review_words(state, cx);
                            (short_review(state).into(), color)
                        }
                    };
                    people_row(&reviewer.who, Some(note))
                }))
                .into_any_element()
        };
        let assignees = if detail.assignees.is_empty() {
            none("No one assigned")
        } else {
            v_flex()
                .gap_1p5()
                .children(
                    detail
                        .assignees
                        .iter()
                        .map(|person| people_row(person, None)),
                )
                .into_any_element()
        };
        let labels = if detail.labels.is_empty() {
            none("None yet")
        } else {
            h_flex()
                .flex_wrap()
                .gap_1()
                .children(detail.labels.iter().map(|label| {
                    let color = label_color(&label.color).unwrap_or(theme.muted_foreground);
                    div()
                        .px_2()
                        .rounded_full()
                        .text_xs()
                        .border_1()
                        .border_color(color)
                        .bg(color.opacity(0.18))
                        .child(label.name.clone())
                }))
                .into_any_element()
        };
        let checks = if detail.checks.is_empty() {
            none("No checks")
        } else {
            // failing first, then running, then passing; a long list is cut
            let mut sorted: Vec<&gh::Check> = detail.checks.iter().collect();
            sorted.sort_by_key(|check| match check.state {
                Checks::Failing => 0,
                Checks::Running => 1,
                Checks::Passing => 2,
            });
            let count = |state: Checks| {
                detail
                    .checks
                    .iter()
                    .filter(|check| check.state == state)
                    .count()
            };
            let summary = [
                (count(Checks::Failing), "failing"),
                (count(Checks::Running), "running"),
                (count(Checks::Passing), "passing"),
            ]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, word)| format!("{n} {word}"))
            .collect::<Vec<_>>()
            .join(" · ");
            // past the first 50 GitHub sends only the count
            let summary = if detail.checks_total > detail.checks.len() as u64 {
                format!("{summary} · {} in all", detail.checks_total)
            } else {
                summary
            };
            let hidden =
                (detail.checks_total as usize).saturating_sub(sorted.len().min(CHECKS_SHOWN));
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(summary),
                )
                .children(sorted.into_iter().take(CHECKS_SHOWN).map(|check| {
                    let (icon, color) = match check.state {
                        Checks::Passing => (IconName::CircleCheck, theme.success),
                        Checks::Failing => (IconName::CircleX, theme.danger),
                        Checks::Running => (IconName::CircleDot, theme.warning),
                    };
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            gpui_kit::component::Icon::new(icon)
                                .xsmall()
                                .text_color(color),
                        )
                        .child(div().min_w_0().truncate().child(check.name.clone()))
                }))
                .when(hidden > 0, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("and {hidden} more")),
                    )
                })
                .into_any_element()
        };
        let decision = match detail.review {
            Review::Approved => Some(("Approved", theme.success)),
            Review::ChangesRequested => Some(("Changes requested", theme.danger)),
            Review::Required => Some(("Review required", theme.muted_foreground)),
            Review::None => None,
        };
        let branch = v_flex()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .truncate()
                    .child(detail.head_ref.clone()),
            )
            .child(
                Button::new("pull-show-head")
                    .outline()
                    .xsmall()
                    .icon(IconName::GitGraph)
                    .label("Show in the graph")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(OverviewEvent::ShowHead))),
            )
            .into_any_element();

        h_flex()
            .size_full()
            .min_h_0()
            .items_start()
            .child(
                v_flex()
                    .id("pull-conversation")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scrollbar()
                    .px_6()
                    .py_4()
                    .gap_3()
                    .children(conversation),
            )
            .child(
                v_flex()
                    .id("pull-facts")
                    .w(gpui_kit::rems(17.))
                    .flex_none()
                    .h_full()
                    .overflow_y_scrollbar()
                    .px_4()
                    .py_4()
                    .gap_3()
                    .border_l_1()
                    .border_color(theme.border)
                    .when_some(decision, |this, (text, color)| {
                        this.child(div().text_sm().font_medium().text_color(color).child(text))
                    })
                    .child(section("Reviewers", reviewers))
                    .child(section("Assignees", assignees))
                    .child(section("Labels", labels))
                    .child(section("Checks", checks))
                    .child(section("Branch", branch)),
            )
            .into_any_element()
    }
}

impl RepoView {
    fn render_pull_files(
        &self,
        pull: &PullView,
        files: &Rc<Vec<PullFile>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        // the detail is shared, so a thread's card reads it in place
        let detail = match &pull.detail {
            Load::Ready(detail) => Some(detail.clone()),
            _ => None,
        };
        let number = pull.number;
        let more_threads = detail.as_ref().map_or(0, |detail| {
            detail
                .threads_total
                .saturating_sub(detail.threads.len() as u64)
        });
        let diff = Diff::new(&pull.diff)
            .line_number(true)
            .annotations(pull.threads.annotations.iter().cloned())
            .render_annotation({
                let detail = detail.clone();
                move |annotation, _, cx| {
                    let gpui_kit::ElementId::NamedInteger(_, ix) = annotation.id() else {
                        return div().into_any_element();
                    };
                    match detail
                        .as_ref()
                        .and_then(|detail| detail.threads.get(*ix as usize))
                    {
                        Some(thread) => thread_card(number, *ix as usize, thread, cx),
                        None => div().into_any_element(),
                    }
                }
            });
        let source = match pull.source {
            Some(DiffSource::Repository) => "From the repository: merge base to head",
            Some(DiffSource::GitHub) => "From GitHub (the commits are not in this repository)",
            None => "",
        };
        let outdated = pull.threads.outdated;
        let thread_counts = pull.threads.per_file.clone();
        let rows = files.clone();
        let selected = pull.selected_file;
        let owner = cx.entity().downgrade();
        let list = div()
            .relative()
            .size_full()
            .child(
                uniform_list("pull-files", rows.len(), move |range, _, cx| {
                    let theme = cx.theme();
                    range
                        .map(|ix| {
                            let file = &rows[ix];
                            let (letter, color) = match file.status {
                                DiffFileStatus::Added => ("A", theme.success),
                                DiffFileStatus::Deleted => ("D", theme.danger),
                                DiffFileStatus::Renamed | DiffFileStatus::Copied => {
                                    ("R", theme.warning)
                                }
                                _ => ("M", theme.warning),
                            };
                            let owner = owner.clone();
                            ListItem::new(("pull-file", ix))
                                // one height for every row, as a uniform list needs
                                .h(gpui_kit::rems(2.75))
                                // a uniform list lays each row out on its own,
                                // as wide as its text unless told: the list's
                                // width, so a long path ends in an ellipsis
                                // instead of pushing the counts out
                                .w_full()
                                .px_3()
                                .selected(selected == Some(ix))
                                .child(
                                    h_flex()
                                        .w_full()
                                        .min_w_0()
                                        .gap_2()
                                        .text_sm()
                                        .child(
                                            div()
                                                .w_3()
                                                .flex_none()
                                                .font_family(theme.mono_font_family.clone())
                                                .text_color(color)
                                                .child(letter),
                                        )
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .child(div().truncate().child(file.name.clone()))
                                                .when(!file.dir.is_empty(), |this| {
                                                    this.child(
                                                        div()
                                                            .truncate()
                                                            .text_xs()
                                                            .text_color(theme.muted_foreground)
                                                            .child(file.dir.clone()),
                                                    )
                                                }),
                                        )
                                        .when_some(
                                            thread_counts.get(&file.path).copied(),
                                            |this, count| {
                                                this.child(
                                                    h_flex()
                                                        .flex_none()
                                                        .gap_0p5()
                                                        .text_xs()
                                                        .text_color(theme.muted_foreground)
                                                        .child(
                                                            gpui_kit::component::Icon::new(
                                                                IconName::MessageSquare,
                                                            )
                                                            .xsmall(),
                                                        )
                                                        .child(count.to_string()),
                                                )
                                            },
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_xs()
                                                .text_color(count_color(
                                                    file.additions,
                                                    theme.success,
                                                    theme.muted_foreground,
                                                ))
                                                .child(format!("+{}", file.additions)),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_xs()
                                                .text_color(count_color(
                                                    file.deletions,
                                                    theme.danger,
                                                    theme.muted_foreground,
                                                ))
                                                .child(format!("−{}", file.deletions)),
                                        ),
                                )
                                .on_click(move |_, _, cx| {
                                    let _ =
                                        owner.update(cx, |view, cx| view.select_pull_file(ix, cx));
                                })
                        })
                        .collect()
                })
                .size_full()
                .py_1()
                .track_scroll(&pull.files_scroll),
            )
            .child(Scrollbar::vertical(&pull.files_scroll));

        let mode = self.diff_mode;
        let body = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                h_flex()
                    .h_8()
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().min_w_0().truncate().child(source))
                    .when(outdated > 0, |this| {
                        this.child(format!(
                            "{outdated} outdated thread{} not shown",
                            if outdated == 1 { "" } else { "s" }
                        ))
                    })
                    .when(more_threads > 0, |this| {
                        this.child(format!("{more_threads} more threads on GitHub"))
                    })
                    .child(
                        TabBar::new("pull-diff-mode")
                            .segmented()
                            .xsmall()
                            .selected_index(usize::from(mode == DiffMode::Split))
                            .on_click(cx.listener(|view, ix: &usize, _, cx| {
                                let mode = if *ix == 1 {
                                    DiffMode::Split
                                } else {
                                    DiffMode::Unified
                                };
                                view.set_diff_mode(mode, cx)
                            }))
                            .child("Unified")
                            .child("Split"),
                    ),
            )
            .child(div().flex_1().min_h_0().child(if files.is_empty() {
                pull_note(Some("No changes.".into()), cx)
            } else {
                self.split_diff("pull-split", diff, &pull.diff, cx)
            }));

        // the divider between the files and the diff moves, as the panels do
        h_resizable("pull-files-split")
            .child(
                resizable_panel()
                    .size(gpui_kit::px(288.))
                    .size_range(gpui_kit::px(160.)..gpui_kit::px(720.))
                    .child(list),
            )
            .child(resizable_panel().child(body))
            .into_any_element()
    }
}

/// GitHub's diff of pull request `number`, fetched and parsed off the UI
/// thread.
async fn github_diff(
    access: Option<(github::GitHubRepo, Option<String>)>,
    token: Option<SharedToken>,
    client: Arc<dyn http_client::HttpClient>,
    number: u64,
    cx: &gpui_kit::AsyncApp,
) -> Result<Vec<DiffFile>, String> {
    let ((repo, _), token) = access
        .zip(token)
        .ok_or_else(|| "GitHub is turned off in Settings.".to_owned())?;
    let token = token
        .await
        .ok_or_else(|| "Sign in with `gh auth login` to see it.".to_owned())?;
    let executor = cx.background_executor().clone();
    cx.background_spawn(async move {
        let text = within(executor, gh::fetch_diff(client, repo, token, number))
            .await
            .map_err(fetch_message)?;
        DiffFile::parse(&text).map_err(|err| format!("{err}"))
    })
    .await
}

/// One token lookup that the detail and the diff both wait on.
type SharedToken = futures::future::Shared<futures::future::BoxFuture<'static, Option<String>>>;

fn fetch_message(err: FetchError) -> String {
    match err {
        FetchError::Unauthorized => "GitHub refused the token.".to_owned(),
        FetchError::Other(err) => format!("{err:#}"),
    }
}

/// The review state's words and colour, as GitHub writes them.
fn review_words(state: ReviewState, cx: &App) -> (&'static str, Hsla) {
    let theme = cx.theme();
    match state {
        ReviewState::Approved => ("approved these changes", theme.success),
        ReviewState::ChangesRequested => ("requested changes", theme.danger),
        ReviewState::Commented => ("reviewed", theme.muted_foreground),
        ReviewState::Dismissed => ("had a review dismissed", theme.muted_foreground),
        ReviewState::Pending => ("started a review", theme.muted_foreground),
    }
}

/// A reviewer's state in the side column, in a word.
fn short_review(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Approved => "Approved",
        ReviewState::ChangesRequested => "Changes",
        ReviewState::Commented => "Commented",
        ReviewState::Dismissed => "Dismissed",
        ReviewState::Pending => "Pending",
    }
}

/// `rrggbb` as GitHub stores label colours.
fn label_color(hex: &str) -> Option<Hsla> {
    let value = u32::from_str_radix(hex.trim_start_matches('#'), 16).ok()?;
    Some(gpui_kit::rgb(value).into())
}

/// "3 hours ago" for an ISO 8601 time from GitHub.
fn ago(at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|time| relative_time(time.with_timezone(&Local), Local::now()))
        .unwrap_or_default()
}

fn pull_note(message: Option<SharedString>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .p_8()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(match message {
            None => Spinner::new().into_any_element(),
            Some(message) => div().max_w_128().child(message).into_any_element(),
        })
        .into_any_element()
}

/// An id for a part of pull request `number`, so another pull request's
/// text does not inherit this one's state (selection, layout).
fn pull_id(number: u64, part: &str, ix: usize) -> gpui_kit::ElementId {
    gpui_kit::ElementId::NamedInteger(format!("pull-{number}-{part}").into(), ix as u64)
}

/// A comment as GitHub draws it: a bordered box with a header line.
fn comment_card(
    id: gpui_kit::ElementId,
    author: &Person,
    verb: &str,
    at: &str,
    body: &str,
    accent: Option<Hsla>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .items_start()
        .gap_3()
        .child(crate::graph::avatar(
            &author.login,
            author.avatar.clone().map(SharedString::from),
            gpui_kit::px(32.),
            theme.border,
            false,
            cx,
        ))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .rounded(theme.radius)
                .border_1()
                .border_color(accent.unwrap_or(theme.border))
                .child(
                    h_flex()
                        .px_3()
                        .py_1p5()
                        .gap_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.muted.opacity(0.35))
                        .text_sm()
                        .child(div().font_semibold().child(author.login.clone()))
                        .child(
                            div()
                                .text_color(accent.unwrap_or(theme.muted_foreground))
                                .child(verb.to_owned()),
                        )
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(format!("· {}", ago(at))),
                        ),
                )
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .child(TextView::markdown(id, body.to_owned()).selectable(true)),
                ),
        )
        .into_any_element()
}

/// A line in the conversation: a review without a body, a merge, a push.
fn event_row(person: &Person, text: &str, at: &str, accent: Option<Hsla>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .pl_11()
        .gap_2()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(crate::graph::avatar(
            &person.login,
            person.avatar.clone().map(SharedString::from),
            gpui_kit::px(18.),
            theme.border,
            false,
            cx,
        ))
        .child(
            div()
                .font_medium()
                .text_color(theme.foreground)
                .child(person.login.clone()),
        )
        .child(
            div()
                .text_color(accent.unwrap_or(theme.muted_foreground))
                .child(text.to_owned()),
        )
        .child(format!("· {}", ago(at)))
        .into_any_element()
}

fn commit_row(oid: &str, headline: &str, author: &Person, at: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .pl_11()
        .gap_2()
        .text_sm()
        .child(
            gpui_kit::component::Icon::new(IconName::GitCommitHorizontal)
                .xsmall()
                .text_color(theme.muted_foreground),
        )
        .child(crate::graph::avatar(
            &author.login,
            author.avatar.clone().map(SharedString::from),
            gpui_kit::px(18.),
            theme.border,
            false,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(headline.to_owned()),
        )
        .child(
            div()
                .flex_none()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(oid.chars().take(7).collect::<String>()),
        )
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(ago(at)),
        )
        .into_any_element()
}

/// A review thread under its line.
fn thread_card(number: u64, ix: usize, thread: &gh::ReviewThread, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .mx_3()
        .my_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .when(thread.resolved, |this| {
            this.child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(theme.success)
                    .border_b_1()
                    .border_color(theme.border)
                    .child("Resolved"),
            )
        })
        .children(thread.comments.iter().enumerate().map(|(cix, comment)| {
            v_flex()
                .px_3()
                .py_2()
                .gap_1()
                .when(cix > 0, |this| this.border_t_1().border_color(theme.border))
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .child(crate::graph::avatar(
                            &comment.author.login,
                            comment.author.avatar.clone().map(SharedString::from),
                            gpui_kit::px(18.),
                            theme.border,
                            false,
                            cx,
                        ))
                        .child(div().font_semibold().child(comment.author.login.clone()))
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(ago(&comment.at)),
                        ),
                )
                .child(
                    div().text_sm().child(
                        TextView::markdown(
                            pull_id(number, &format!("thread-{ix}"), cix),
                            comment.body.clone(),
                        )
                        .selectable(true),
                    ),
                )
        }))
        .when(
            thread.comments_total > thread.comments.len() as u64,
            |this| {
                let more = thread.comments_total - thread.comments.len() as u64;
                this.child(
                    div()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{more} more repl{} on GitHub",
                            if more == 1 { "y" } else { "ies" }
                        )),
                )
            },
        )
        .into_any_element()
}

/// A line saying part of the pull request is only on GitHub, with a link.
fn on_github_row(text: &str, url: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let url = url.to_owned();
    h_flex()
        .pl_11()
        .gap_2()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(text.to_owned())
        .child(
            Button::new("pull-earlier-on-github")
                .ghost()
                .xsmall()
                .icon(IconName::ExternalLink)
                .label("See them on GitHub")
                .on_click(move |_, _, cx| cx.open_url(&url)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(path: &str, line: Option<u64>) -> gh::ReviewThread {
        gh::ReviewThread {
            path: path.to_owned(),
            line,
            side: Side::Right,
            resolved: false,
            comments: Vec::new(),
            comments_total: 0,
        }
    }

    #[test]
    fn threads_are_indexed_by_file_and_line() {
        let threads = [
            thread("src/a.rs", Some(3)),
            thread("src/a.rs", None),
            thread("b.md", Some(1)),
        ];
        let index = ThreadIndex::of(&threads);
        assert_eq!(index.per_file.get("src/a.rs"), Some(&2));
        assert_eq!(index.per_file.get("b.md"), Some(&1));
        assert_eq!(index.outdated, 1);
        // the outdated thread has no line to sit under; the ids keep the
        // threads' own places, which the cards are looked up by
        let ids: Vec<_> = index
            .annotations
            .iter()
            .map(|annotation| annotation.id().clone())
            .collect();
        assert_eq!(
            ids,
            vec![
                gpui_kit::ElementId::from(("thread", 0usize)),
                gpui_kit::ElementId::from(("thread", 2usize)),
            ]
        );
    }

    #[test]
    fn reviewers_are_summed_up_in_a_word() {
        assert_eq!(short_review(ReviewState::Approved), "Approved");
        assert_eq!(short_review(ReviewState::ChangesRequested), "Changes");
    }
}
