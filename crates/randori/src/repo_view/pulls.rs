//! The "Pull requests" section of the side panel: open pull requests of the
//! workspace's GitHub remote, refreshed in the background, plus the author
//! avatars the same query brings back for the graph.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui_kit::component::{WindowExt as _, notification::Notification};

use super::*;
use crate::github::{self, Checks, FetchError, GitHubRepo, PullRequest, Review};

/// Background refresh while the tab is open.
const REFRESH_EVERY: Duration = Duration::from_secs(60);
/// Window focus and tab switches refresh at most this often.
const MIN_INTERVAL: Duration = Duration::from_secs(30);

pub(super) enum PullsState {
    Loading,
    /// No token for the host, or the host refused it.
    SignedOut,
    Failed(SharedString),
    Ready,
}

pub(super) struct Pulls {
    repo: GitHubRepo,
    state: PullsState,
    list: Vec<PullRequest>,
    total: usize,
    expanded: bool,
    /// Kept in memory only, for the next refresh.
    token: Option<String>,
    fetching: bool,
    last_fetch: Option<Instant>,
    /// Avatar URL → whether it is a real picture (not an identicon).
    avatar_checks: HashMap<String, bool>,
    _timer: Task<()>,
}

impl RepoView {
    /// Starts (or keeps) the pull request section for the workspace's GitHub
    /// remote; workspaces without one show no section.
    pub(super) fn start_pulls(&mut self, remotes: &[(String, String)], cx: &mut Context<Self>) {
        let Some(repo) = GitHubRepo::pick(remotes) else {
            self.pulls = None;
            return;
        };
        if self.pulls.as_ref().is_some_and(|pulls| pulls.repo == repo) {
            self.refresh_pulls(false, cx);
            return;
        }
        let _timer = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH_EVERY).await;
                if this
                    .update(cx, |view, cx| view.refresh_pulls(true, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.pulls = Some(Pulls {
            repo,
            state: PullsState::Loading,
            list: Vec::new(),
            total: 0,
            expanded: true,
            token: None,
            fetching: false,
            last_fetch: None,
            avatar_checks: HashMap::new(),
            _timer,
        });
        self.refresh_pulls(true, cx);
    }

    /// Fetches the pull requests again; without `force`, only when the last
    /// fetch is older than [`MIN_INTERVAL`].
    pub(super) fn refresh_pulls(&mut self, force: bool, cx: &mut Context<Self>) {
        let client = cx.http_client();
        let Some(pulls) = &mut self.pulls else {
            return;
        };
        let recent = pulls
            .last_fetch
            .is_some_and(|at| at.elapsed() < MIN_INTERVAL);
        if pulls.fetching || (!force && recent) {
            return;
        }
        pulls.fetching = true;
        let repo = pulls.repo.clone();
        let cached = pulls.token.clone();
        let known: HashSet<String> = pulls.avatar_checks.keys().cloned().collect();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let token = match cached {
                Some(token) => Some(token),
                None => {
                    let host = repo.host.clone();
                    cx.background_spawn(async move { github::find_token(&host) })
                        .await
                }
            };
            let result = match token.clone() {
                Some(token) => match github::fetch(client.clone(), repo, token).await {
                    Ok(snapshot) => {
                        // look at each new picture once, to drop identicons
                        let mut urls: Vec<String> = snapshot
                            .avatars
                            .values()
                            .cloned()
                            .chain(
                                snapshot
                                    .pulls
                                    .iter()
                                    .filter_map(|pr| pr.author_avatar.clone()),
                            )
                            .filter(|url| !known.contains(url))
                            .collect();
                        urls.sort();
                        urls.dedup();
                        let checks = github::check_avatars(client, urls).await;
                        Some(Ok((snapshot, checks)))
                    }
                    Err(err) => Some(Err(err)),
                },
                None => None,
            };
            let _ = this.update(cx, |view, cx| {
                let Some(pulls) = &mut view.pulls else {
                    return;
                };
                pulls.fetching = false;
                pulls.last_fetch = Some(Instant::now());
                match result {
                    Some(Ok((snapshot, checks))) => {
                        pulls.avatar_checks.extend(checks);
                        let real = |url: &String| pulls.avatar_checks.get(url) != Some(&false);
                        pulls.token = token;
                        pulls.list = snapshot.pulls;
                        for pr in &mut pulls.list {
                            pr.author_avatar = pr.author_avatar.take().filter(|url| real(url));
                        }
                        pulls.total = snapshot.total;
                        pulls.state = PullsState::Ready;
                        let mut avatars = (*view.avatars).clone();
                        for (email, url) in snapshot.avatars {
                            if real(&url) {
                                avatars.insert(email, url.into());
                            } else {
                                avatars.remove(&email);
                            }
                        }
                        view.avatars = Rc::new(avatars);
                    }
                    None | Some(Err(FetchError::Unauthorized)) => {
                        pulls.token = None;
                        pulls.state = PullsState::SignedOut;
                    }
                    Some(Err(FetchError::Other(err))) => {
                        log::warn!("pull requests: {err:#}");
                        pulls.state = PullsState::Failed(format!("{err:#}").into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn pulls_fetching(&self) -> bool {
        self.pulls.as_ref().is_some_and(|pulls| pulls.fetching)
    }

    /// Whether GitHub avatars apply to this workspace (signed in to its host).
    pub(super) fn github_active(&self) -> bool {
        self.pulls
            .as_ref()
            .is_some_and(|pulls| matches!(pulls.state, PullsState::Ready))
    }

    /// Selects the head revision of pull request `ix`. A head outside the
    /// loaded revisions switches to "All branches" and selects it there.
    fn select_pull(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pr) = self.pulls.as_ref().and_then(|pulls| pulls.list.get(ix)) else {
            return;
        };
        let oid = pr.head_oid.clone();
        if let Some(row) = self
            .rows
            .iter()
            .position(|row| row.revision.id.commit.hex == oid)
        {
            self.select(row, cx);
            self.log_focus.focus(window, cx);
        } else if self.log_scope() != Some(true) {
            self.pending_commit_select = Some(oid);
            self.set_log_scope(true, window, cx);
        } else {
            let (number, head) = (pr.number, pr.head_ref.clone());
            window.push_notification(
                Notification::info(format!(
                    "Fetch to bring in {head}, or the head may be in a fork."
                ))
                .title(format!("#{number}'s head is not in this repository")),
                cx,
            );
        }
    }

    pub(super) fn render_pulls(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pulls = self.pulls.as_ref()?;
        let theme = cx.theme();
        let selected_commit = self
            .selected
            .and_then(|ix| self.rows.get(ix))
            .map(|row| row.revision.id.commit.hex.clone());

        let header = h_flex()
            .id("pulls-header")
            .h_10()
            .px_4()
            .gap_2()
            .flex_none()
            .cursor_pointer()
            .on_click(cx.listener(|view, _, _, cx| {
                if let Some(pulls) = &mut view.pulls {
                    pulls.expanded = !pulls.expanded;
                }
                cx.notify();
            }))
            .child(
                gpui_kit::component::Icon::new(if pulls.expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(theme.muted_foreground),
            )
            .child(div().text_sm().font_medium().child("Pull requests"))
            .when(matches!(pulls.state, PullsState::Ready), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(pulls.total.to_string()),
                )
            })
            .child(div().flex_1())
            .when(pulls.fetching, |this| this.child(Spinner::new().xsmall()))
            .child(
                Button::new("pulls-refresh")
                    .ghost()
                    .xsmall()
                    .icon(IconName::RefreshCw)
                    .tooltip("Refresh pull requests")
                    .disabled(pulls.fetching)
                    .on_click(cx.listener(|view, _, _, cx| {
                        cx.stop_propagation();
                        view.refresh_pulls(true, cx)
                    })),
            );

        let note = |text: SharedString| {
            div()
                .px_4()
                .pb_3()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let body: AnyElement = match &pulls.state {
            PullsState::Loading => note("Loading pull requests…".into()).into_any_element(),
            PullsState::SignedOut => {
                let host = &pulls.repo.host;
                let command = if host == "github.com" {
                    "gh auth login".to_owned()
                } else {
                    format!("gh auth login --hostname {host}")
                };
                v_flex()
                    .px_4()
                    .pb_3()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("Sign in to {host} to see pull requests:"))
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.foreground)
                            .child(command),
                    )
                    .child("or set GH_TOKEN, then refresh.")
                    .into_any_element()
            }
            PullsState::Failed(message) => v_flex()
                .px_4()
                .pb_3()
                .gap_1()
                .text_xs()
                .child(div().text_color(theme.danger).child(message.clone()))
                .into_any_element(),
            PullsState::Ready if pulls.list.is_empty() => {
                note("No open pull requests.".into()).into_any_element()
            }
            PullsState::Ready => v_flex()
                .id("pulls")
                .max_h(gpui_kit::rems(18.))
                .overflow_y_scrollbar()
                .pb_1()
                .children(pulls.list.iter().enumerate().map(|(ix, pr)| {
                    let selected = selected_commit.as_deref() == Some(pr.head_oid.as_str());
                    render_pull(pr, ix, selected, cx)
                        .on_click(
                            cx.listener(move |view, _, window, cx| {
                                view.select_pull(ix, window, cx)
                            }),
                        )
                        .into_any_element()
                }))
                .when(pulls.total > pulls.list.len(), |this| {
                    let url = format!("{}/pulls", pulls.repo.web_url());
                    this.child(
                        div().px_4().py_1().child(
                            Button::new("pulls-more")
                                .link()
                                .xsmall()
                                .label(format!("{} more on GitHub", pulls.total - pulls.list.len()))
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        ),
                    )
                })
                .into_any_element(),
        };

        Some(
            v_flex()
                .flex_none()
                .child(header)
                .when(pulls.expanded, |this| this.child(body))
                .child(div().h_px().bg(theme.border))
                .into_any_element(),
        )
    }
}

/// One pull request: author avatar, number and title, then branches, review
/// and checks, with a button that opens it on GitHub.
fn render_pull(pr: &PullRequest, ix: usize, selected: bool, cx: &App) -> ListItem {
    let theme = cx.theme();
    let (review, review_color) = match pr.review {
        Review::Approved => (Some("Approved"), theme.success),
        Review::ChangesRequested => (Some("Changes requested"), theme.danger),
        Review::Required => (Some("Review required"), theme.muted_foreground),
        Review::None => (None, theme.muted_foreground),
    };
    let checks = pr.checks.map(|checks| match checks {
        Checks::Passing => (IconName::CircleCheck, theme.success, "Checks passed"),
        Checks::Failing => (IconName::CircleX, theme.danger, "Checks failed"),
        Checks::Running => (IconName::CircleDot, theme.warning, "Checks running"),
    });
    let url = pr.url.clone();
    ListItem::new(("pull", ix))
        .px_4()
        .py_1p5()
        .selected(selected)
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_3()
                .child(crate::graph::avatar(
                    &pr.author,
                    pr.author_avatar.clone().map(SharedString::from),
                    gpui_kit::px(22.),
                    theme.border,
                    false,
                    cx,
                ))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .gap_1p5()
                                .min_w_0()
                                .text_sm()
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("#{}", pr.number)),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_medium()
                                        .when(pr.draft, |this| {
                                            this.text_color(theme.muted_foreground)
                                        })
                                        .child(pr.title.clone()),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_1p5()
                                .min_w_0()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .when(pr.draft, |this| this.child(div().child("Draft ·")))
                                .child(div().flex_none().child(pr.author.clone()))
                                .child("·")
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(theme.mono_font_family.clone())
                                        .child(format!("{} → {}", pr.head_ref, pr.base_ref)),
                                )
                                .when_some(review, |this, review| {
                                    this.child(
                                        div().flex_none().text_color(review_color).child(review),
                                    )
                                }),
                        ),
                )
                .when_some(checks, |this, (icon, color, label)| {
                    this.child(
                        div()
                            .id(("pull-checks", ix))
                            .flex_none()
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(label).build(window, cx)
                            })
                            .child(
                                gpui_kit::component::Icon::new(icon)
                                    .small()
                                    .text_color(color),
                            ),
                    )
                })
                .child(
                    Button::new(("pull-open", ix))
                        .ghost()
                        .xsmall()
                        .icon(IconName::ExternalLink)
                        .tooltip("Open on GitHub")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            cx.open_url(&url)
                        }),
                ),
        )
}
