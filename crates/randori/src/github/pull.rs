//! One pull request in detail, as its page on GitHub shows it: the
//! description, the conversation, reviewers, labels and checks, its review
//! threads, and its diff. Also the avatars of commit authors the pull request
//! query did not bring.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use http_client::{AsyncBody, HttpClient, Method};
use serde_json::{Value, json};

use super::{Checks, FetchError, GitHubRepo, Review, graphql, send};

#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub login: String,
    pub avatar: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PullState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reviewer {
    /// A user, or a team by name (no picture).
    pub who: Person,
    /// The latest review; `None` while it is only requested.
    pub state: Option<ReviewState>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub name: String,
    /// `rrggbb`, as GitHub stores it.
    pub color: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub state: Checks,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TimelineItem {
    Comment {
        author: Person,
        body: String,
        at: String,
    },
    Review {
        author: Person,
        state: ReviewState,
        body: String,
        at: String,
    },
    Commit {
        oid: String,
        headline: String,
        author: Person,
        at: String,
    },
    /// Merged, closed, reopened, force-pushed: one line each.
    Event {
        actor: Person,
        text: String,
        at: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadComment {
    pub author: Person,
    pub body: String,
    pub at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewThread {
    pub path: String,
    /// The line it is on now; `None` when outdated (the code moved on).
    pub line: Option<u64>,
    pub side: Side,
    pub resolved: bool,
    pub comments: Vec<ThreadComment>,
    /// All the thread's comments; more than `comments` when it is long.
    pub comments_total: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PullDetail {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub body: String,
    pub state: PullState,
    pub author: Person,
    pub created_at: String,
    pub head_ref: String,
    pub base_ref: String,
    pub head_oid: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub review: Review,
    pub checks: Vec<Check>,
    /// All the head commit's checks; more than `checks` past the first 50.
    pub checks_total: u64,
    pub labels: Vec<Label>,
    pub assignees: Vec<Person>,
    pub reviewers: Vec<Reviewer>,
    /// The latest 100 items of the conversation, oldest first.
    pub timeline: Vec<TimelineItem>,
    /// All the conversation's items; more than `timeline` on a long one.
    pub timeline_total: u64,
    pub threads: Vec<ReviewThread>,
    /// All the review threads; more than `threads` past the first 100.
    pub threads_total: u64,
}

const PERSON: &str = "login avatarUrl(size: 64)";

fn detail_query() -> String {
    format!(
        "query($owner: String!, $name: String!, $number: Int!) {{
  repository(owner: $owner, name: $name) {{
    pullRequest(number: $number) {{
      number title url body state isDraft createdAt
      author {{ {PERSON} }}
      headRefName baseRefName headRefOid
      additions deletions changedFiles reviewDecision
      labels(first: 20) {{ nodes {{ name color }} }}
      assignees(first: 10) {{ nodes {{ {PERSON} }} }}
      reviewRequests(first: 20) {{ nodes {{ requestedReviewer {{
        ... on User {{ {PERSON} }}
        ... on Team {{ name }}
      }} }} }}
      latestReviews(first: 20) {{ nodes {{ state author {{ {PERSON} }} }} }}
      commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ contexts(first: 50) {{ totalCount nodes {{
        __typename
        ... on CheckRun {{ name status conclusion }}
        ... on StatusContext {{ context state }}
      }} }} }} }} }} }}
      timelineItems(last: 100, itemTypes: [ISSUE_COMMENT, PULL_REQUEST_REVIEW, PULL_REQUEST_COMMIT,
          MERGED_EVENT, CLOSED_EVENT, REOPENED_EVENT, HEAD_REF_FORCE_PUSHED_EVENT]) {{ totalCount nodes {{
        __typename
        ... on IssueComment {{ author {{ {PERSON} }} body createdAt }}
        ... on PullRequestReview {{ author {{ {PERSON} }} body state createdAt }}
        ... on PullRequestCommit {{ commit {{ oid messageHeadline committedDate
          author {{ name user {{ {PERSON} }} }} }} }}
        ... on MergedEvent {{ actor {{ {PERSON} }} createdAt }}
        ... on ClosedEvent {{ actor {{ {PERSON} }} createdAt }}
        ... on ReopenedEvent {{ actor {{ {PERSON} }} createdAt }}
        ... on HeadRefForcePushedEvent {{ actor {{ {PERSON} }} createdAt }}
      }} }}
      reviewThreads(first: 100) {{ totalCount nodes {{
        path line diffSide isResolved
        comments(first: 50) {{ totalCount nodes {{ author {{ {PERSON} }} body createdAt }} }}
      }} }}
    }}
  }}
}}"
    )
}

/// Everything the pull request page shows, but the diff.
pub async fn fetch_detail(
    client: Arc<dyn HttpClient>,
    repo: GitHubRepo,
    token: String,
    number: u64,
) -> Result<PullDetail, FetchError> {
    let variables = json!({ "owner": repo.owner, "name": repo.name, "number": number });
    let value = graphql(&client, &repo, &token, &detail_query(), variables).await?;
    Ok(parse_detail(&value)?)
}

/// The pull request's diff as GitHub computes it (merge base to head), for
/// when its commits are not in the repository. GitHub refuses very large ones.
pub async fn fetch_diff(
    client: Arc<dyn HttpClient>,
    repo: GitHubRepo,
    token: String,
    number: u64,
) -> Result<String, FetchError> {
    let url = repo.rest_url(&format!(
        "/repos/{}/{}/pulls/{number}",
        repo.owner, repo.name
    ));
    let bytes = send(
        &client,
        Method::GET,
        &url,
        &token,
        "application/vnd.github.diff",
        AsyncBody::empty(),
    )
    .await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// How many commits one avatar query asks about.
const AVATAR_BATCH: usize = 50;

/// What the avatar batches answered.
#[derive(Default)]
pub struct CommitAvatars {
    /// Email (lowercase) → picture.
    pub found: HashMap<String, String>,
    /// The emails of batches that failed, to ask again later.
    pub unanswered: Vec<String>,
    /// Why the first failed batch failed.
    pub error: Option<FetchError>,
}

/// Avatars of commit authors, by email (lowercase), asked about one commit of
/// each: GitHub answers with the account's picture, or its default one when
/// the email belongs to no account. Commits GitHub does not have are skipped.
/// A batch that fails leaves the others' answers in place.
pub async fn fetch_commit_avatars(
    client: Arc<dyn HttpClient>,
    repo: GitHubRepo,
    token: String,
    commits: Vec<(String, String)>,
) -> Result<CommitAvatars, FetchError> {
    // the batches go out together
    let answers = futures::future::join_all(commits.chunks(AVATAR_BATCH).map(|batch| {
        let (client, repo, token) = (&client, &repo, &token);
        async move {
            let value = graphql(
                client,
                repo,
                token,
                &avatar_query(batch),
                json!({ "owner": repo.owner, "name": repo.name }),
            )
            .await?;
            Ok::<_, FetchError>(parse_avatars(&value, batch)?)
        }
    }))
    .await;
    let mut avatars = CommitAvatars::default();
    for (answer, batch) in answers.into_iter().zip(commits.chunks(AVATAR_BATCH)) {
        match answer {
            Ok(found) => avatars.found.extend(found),
            Err(err) => {
                avatars
                    .unanswered
                    .extend(batch.iter().map(|(email, _)| email.clone()));
                avatars.error.get_or_insert(err);
            }
        }
    }
    Ok(avatars)
}

fn avatar_query(batch: &[(String, String)]) -> String {
    let fields: String = batch
        .iter()
        .enumerate()
        .filter(|(_, (_, oid))| oid.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|(ix, (_, oid))| {
            format!(
                "c{ix}: object(oid: \"{oid}\") {{ ... on Commit {{ author {{ email avatarUrl(size: 64) }} }} }}\n"
            )
        })
        .collect();
    format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{\n{fields}}} }}"
    )
}

/// Reads an answer to [`avatar_query`]. A batch without the repository
/// fails, so its emails are asked again; a commit GitHub does not have is
/// left out.
fn parse_avatars(value: &Value, batch: &[(String, String)]) -> Result<HashMap<String, String>> {
    let repo = super::answered(
        value,
        &["repository"],
        "the repository is not visible to this GitHub account",
    )?;
    Ok(batch
        .iter()
        .enumerate()
        .filter_map(|(ix, (email, _))| {
            let url = repo[format!("c{ix}")]["author"]["avatarUrl"].as_str()?;
            Some((email.to_ascii_lowercase(), url.to_owned()))
        })
        .collect())
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn person(value: &Value) -> Person {
    Person {
        // a deleted account comes back as null
        login: value["login"].as_str().unwrap_or("ghost").to_owned(),
        avatar: value["avatarUrl"].as_str().map(str::to_owned),
    }
}

fn nodes(value: &Value) -> impl Iterator<Item = &Value> {
    value["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| !node.is_null())
}

/// A connection's `totalCount`; 0 when GitHub left it out.
fn total(connection: &Value) -> u64 {
    connection["totalCount"].as_u64().unwrap_or_default()
}

fn review_state(value: &Value) -> Option<ReviewState> {
    Some(match value.as_str()? {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "COMMENTED" => ReviewState::Commented,
        "DISMISSED" => ReviewState::Dismissed,
        "PENDING" => ReviewState::Pending,
        _ => return None,
    })
}

/// Reads an answer to [`detail_query`].
pub fn parse_detail(value: &Value) -> Result<PullDetail> {
    let pr = super::answered(
        value,
        &["repository", "pullRequest"],
        "the pull request is not visible to this GitHub account",
    )?;
    let state = match (pr["state"].as_str(), pr["isDraft"].as_bool()) {
        (Some("MERGED"), _) => PullState::Merged,
        (Some("CLOSED"), _) => PullState::Closed,
        (_, Some(true)) => PullState::Draft,
        _ => PullState::Open,
    };

    let mut reviewers: Vec<Reviewer> = nodes(&pr["latestReviews"])
        .filter_map(|review| {
            Some(Reviewer {
                who: person(&review["author"]),
                state: Some(review_state(&review["state"])?),
            })
        })
        .collect();
    for request in nodes(&pr["reviewRequests"]) {
        let reviewer = &request["requestedReviewer"];
        let who = match reviewer["name"].as_str() {
            Some(team) => Person {
                login: team.to_owned(),
                avatar: None,
            },
            None => person(reviewer),
        };
        // a reviewer asked again after reviewing shows as requested
        reviewers.retain(|known| known.who.login != who.login);
        reviewers.push(Reviewer { who, state: None });
    }

    let checks: Vec<Check> =
        nodes(&pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"])
            .filter_map(|context| {
                let (name, state) = match context["__typename"].as_str()? {
                    "CheckRun" => (
                        text(&context["name"]),
                        match (context["status"].as_str(), context["conclusion"].as_str()) {
                            (Some("COMPLETED"), Some("SUCCESS" | "NEUTRAL" | "SKIPPED")) => {
                                Checks::Passing
                            }
                            (Some("COMPLETED"), _) => Checks::Failing,
                            _ => Checks::Running,
                        },
                    ),
                    "StatusContext" => (
                        text(&context["context"]),
                        match context["state"].as_str() {
                            Some("SUCCESS") => Checks::Passing,
                            Some("FAILURE" | "ERROR") => Checks::Failing,
                            _ => Checks::Running,
                        },
                    ),
                    _ => return None,
                };
                Some(Check { name, state })
            })
            .collect();

    let timeline: Vec<TimelineItem> = nodes(&pr["timelineItems"])
        .filter_map(|item| {
            let event = |text: &str| TimelineItem::Event {
                actor: person(&item["actor"]),
                text: text.to_owned(),
                at: self::text(&item["createdAt"]),
            };
            Some(match item["__typename"].as_str()? {
                "IssueComment" => TimelineItem::Comment {
                    author: person(&item["author"]),
                    body: text(&item["body"]),
                    at: text(&item["createdAt"]),
                },
                "PullRequestReview" => TimelineItem::Review {
                    author: person(&item["author"]),
                    state: review_state(&item["state"])?,
                    body: text(&item["body"]),
                    at: text(&item["createdAt"]),
                },
                "PullRequestCommit" => {
                    let commit = &item["commit"];
                    let user = &commit["author"]["user"];
                    TimelineItem::Commit {
                        oid: text(&commit["oid"]),
                        headline: text(&commit["messageHeadline"]),
                        author: if user.is_null() {
                            Person {
                                login: text(&commit["author"]["name"]),
                                avatar: None,
                            }
                        } else {
                            person(user)
                        },
                        at: text(&commit["committedDate"]),
                    }
                }
                "MergedEvent" => event("merged this pull request"),
                "ClosedEvent" => event("closed this pull request"),
                "ReopenedEvent" => event("reopened this pull request"),
                "HeadRefForcePushedEvent" => event("force-pushed the branch"),
                _ => return None,
            })
        })
        .collect();

    let threads = nodes(&pr["reviewThreads"])
        .map(|thread| ReviewThread {
            path: text(&thread["path"]),
            line: thread["line"].as_u64(),
            side: if thread["diffSide"].as_str() == Some("LEFT") {
                Side::Left
            } else {
                Side::Right
            },
            resolved: thread["isResolved"].as_bool().unwrap_or_default(),
            comments: nodes(&thread["comments"])
                .map(|comment| ThreadComment {
                    author: person(&comment["author"]),
                    body: text(&comment["body"]),
                    at: text(&comment["createdAt"]),
                })
                .collect(),
            comments_total: total(&thread["comments"]),
        })
        .collect::<Vec<_>>();
    let checks_context = &pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"];

    Ok(PullDetail {
        number: pr["number"].as_u64().unwrap_or_default(),
        title: text(&pr["title"]),
        url: text(&pr["url"]),
        body: text(&pr["body"]),
        state,
        author: person(&pr["author"]),
        created_at: text(&pr["createdAt"]),
        head_ref: text(&pr["headRefName"]),
        base_ref: text(&pr["baseRefName"]),
        head_oid: text(&pr["headRefOid"]),
        additions: pr["additions"].as_u64().unwrap_or_default(),
        deletions: pr["deletions"].as_u64().unwrap_or_default(),
        changed_files: pr["changedFiles"].as_u64().unwrap_or_default(),
        review: match pr["reviewDecision"].as_str() {
            Some("APPROVED") => Review::Approved,
            Some("CHANGES_REQUESTED") => Review::ChangesRequested,
            Some("REVIEW_REQUIRED") => Review::Required,
            _ => Review::None,
        },
        checks_total: total(checks_context).max(checks.len() as u64),
        checks,
        labels: nodes(&pr["labels"])
            .map(|label| Label {
                name: text(&label["name"]),
                color: text(&label["color"]),
            })
            .collect(),
        assignees: nodes(&pr["assignees"]).map(person).collect(),
        reviewers,
        timeline_total: total(&pr["timelineItems"]).max(timeline.len() as u64),
        timeline,
        threads_total: total(&pr["reviewThreads"]).max(threads.len() as u64),
        threads,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pull_request_reads_like_its_page() {
        let value: Value = serde_json::from_str(
            r###"{ "data": { "repository": { "pullRequest": {
            "number": 123, "title": "fix: lists", "url": "https://github.com/o/r/pull/123",
            "body": "## Summary\n- one", "state": "OPEN", "isDraft": false,
            "createdAt": "2026-11-01T00:00:00Z",
            "author": { "login": "van", "avatarUrl": "https://a/van" },
            "headRefName": "van/qa", "baseRefName": "qa/2.0.0",
            "headRefOid": "aa",
            "additions": 10, "deletions": 2, "changedFiles": 3,
            "reviewDecision": "REVIEW_REQUIRED",
            "labels": { "nodes": [{ "name": "bug", "color": "d73a4a" }] },
            "assignees": { "nodes": [] },
            "reviewRequests": { "nodes": [
                { "requestedReviewer": { "login": "ray", "avatarUrl": "https://a/ray" } },
                { "requestedReviewer": { "name": "player-team" } }
            ] },
            "latestReviews": { "nodes": [
                { "state": "APPROVED", "author": { "login": "ian", "avatarUrl": null } },
                { "state": "COMMENTED", "author": { "login": "ray", "avatarUrl": "https://a/ray" } }
            ] },
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": { "totalCount": 70, "nodes": [
                { "__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS" },
                { "__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS", "conclusion": null },
                { "__typename": "StatusContext", "context": "lint", "state": "FAILURE" }
            ] } } } }] },
            "timelineItems": { "totalCount": 250, "nodes": [
                { "__typename": "PullRequestCommit", "commit": { "oid": "aa",
                  "messageHeadline": "fix it", "committedDate": "2026-11-01T01:00:00Z",
                  "author": { "name": "Van", "user": null } } },
                { "__typename": "IssueComment", "author": null, "body": "hi",
                  "createdAt": "2026-11-01T02:00:00Z" },
                { "__typename": "MergedEvent", "actor": { "login": "ian", "avatarUrl": null },
                  "createdAt": "2026-11-02T00:00:00Z" }
            ] },
            "reviewThreads": { "totalCount": 120, "nodes": [
                { "path": "src/a.rs", "line": 7, "diffSide": "RIGHT", "isResolved": false,
                  "comments": { "totalCount": 60, "nodes": [{ "author": { "login": "ray", "avatarUrl": null },
                    "body": "why?", "createdAt": "2026-11-01T03:00:00Z" }] } },
                { "path": "src/b.rs", "line": null, "diffSide": "LEFT", "isResolved": true,
                  "comments": { "nodes": [] } }
            ] }
        } } } }"###,
        )
        .unwrap();
        let pr = parse_detail(&value).unwrap();
        assert_eq!(pr.number, 123);
        assert_eq!(pr.state, PullState::Open);
        assert_eq!(pr.labels[0].name, "bug");
        // ray reviewed and was asked again: requested, once
        let ray: Vec<_> = pr
            .reviewers
            .iter()
            .filter(|r| r.who.login == "ray")
            .collect();
        assert_eq!(ray.len(), 1);
        assert_eq!(ray[0].state, None);
        assert!(
            pr.reviewers
                .iter()
                .any(|r| r.who.login == "ian" && r.state == Some(ReviewState::Approved))
        );
        assert!(pr.reviewers.iter().any(|r| r.who.login == "player-team"));
        assert_eq!(
            pr.checks.iter().map(|c| c.state).collect::<Vec<_>>(),
            [Checks::Passing, Checks::Running, Checks::Failing]
        );
        assert_eq!(pr.timeline.len(), 3);
        assert!(
            matches!(&pr.timeline[1], TimelineItem::Comment { author, .. } if author.login == "ghost")
        );
        assert!(
            matches!(&pr.timeline[0], TimelineItem::Commit { author, .. } if author.login == "Van")
        );
        assert_eq!(pr.threads[0].line, Some(7));
        // what GitHub only counts: the view says it is there
        assert_eq!(
            (pr.timeline_total, pr.threads_total, pr.checks_total),
            (250, 120, 70)
        );
        assert_eq!(pr.threads[0].comments_total, 60);
        // a connection without a count is as long as what came
        assert_eq!(pr.threads[1].comments_total, 0);
        assert_eq!(pr.threads[1].side, Side::Left);
    }

    #[test]
    fn a_field_the_token_cannot_read_leaves_the_rest() {
        // GitHub nulls the checks and says why; the page still loads
        let partial = json!({
            "data": { "repository": { "pullRequest": {
                "number": 7, "title": "T", "state": "OPEN",
                "commits": { "nodes": [ { "commit": { "statusCheckRollup": null } } ] }
            } } },
            "errors": [ { "message": "Resource not accessible by integration" } ]
        });
        let detail = parse_detail(&partial).expect("the rest of the page");
        assert_eq!((detail.number, detail.title.as_str()), (7, "T"));
        assert!(detail.checks.is_empty());
    }

    #[test]
    fn missing_pull_requests_and_errors_are_errors() {
        let error = json!({ "errors": [{ "message": "Could not resolve" }] });
        assert!(parse_detail(&error).is_err());
        let gone = json!({ "data": { "repository": { "pullRequest": null } } });
        assert!(parse_detail(&gone).is_err());
    }

    #[test]
    fn commit_avatars_are_asked_by_alias() {
        let batch = vec![
            ("Van@Example.com".to_owned(), "abc123".to_owned()),
            ("ray@example.com".to_owned(), "not hex!".to_owned()),
            ("ian@example.com".to_owned(), "def456".to_owned()),
        ];
        let query = avatar_query(&batch);
        assert!(query.contains("c0: object(oid: \"abc123\")"));
        assert!(!query.contains("not hex"), "only hex ids reach the query");
        let value = json!({ "data": { "repository": {
            "c0": { "author": { "email": "Van@Example.com", "avatarUrl": "https://a/van" } },
            "c2": null
        } } });
        let avatars = parse_avatars(&value, &batch).unwrap();
        // no repository: the batch fails, to be asked again
        let hidden = json!({ "data": { "repository": null } });
        assert!(parse_avatars(&hidden, &batch).is_err());
        assert_eq!(
            avatars.get("van@example.com").map(String::as_str),
            Some("https://a/van")
        );
        assert_eq!(avatars.len(), 1, "commits GitHub does not have are skipped");
    }
}
