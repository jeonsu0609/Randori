//! GitHub integration: open pull requests and author avatars for a
//! workspace whose remote is on GitHub (github.com or GitHub Enterprise).
//!
//! The token comes from `GH_TOKEN` / `GITHUB_TOKEN` or the GitHub CLI
//! (`gh auth token`). Randori never stores it, never logs it, and sends it
//! only to that host's GraphQL API over HTTPS.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient, Method, Request, StatusCode};
use serde_json::{Value, json};

/// A repository on a GitHub host.
#[derive(Clone, Debug, PartialEq)]
pub struct GitHubRepo {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl GitHubRepo {
    /// Reads `https://host/owner/name(.git)`, `git@host:owner/name(.git)` and
    /// `ssh://git@host(:port)/owner/name(.git)`. Hosts other than github.com
    /// count only when they look like GitHub Enterprise (`github.` in the name).
    pub fn from_remote_url(url: &str) -> Option<Self> {
        let url = url.trim();
        let (host, path) = if let Some(rest) = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .or_else(|| url.strip_prefix("ssh://"))
            .or_else(|| url.strip_prefix("git://"))
        {
            let rest = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
            rest.split_once('/')?
        } else {
            // scp-like: [user@]host:owner/name
            let rest = url.rsplit_once('@').map_or(url, |(_, host)| host);
            rest.split_once(':')?
        };
        let host = host.split(':').next()?.to_ascii_lowercase();
        if host != "github.com" && !host.contains("github.") {
            return None;
        }
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, name) = path.split_once('/')?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return None;
        }
        Some(Self {
            host,
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// `origin` if it is on GitHub, otherwise the first remote that is.
    pub fn pick(remotes: &[(String, String)]) -> Option<Self> {
        remotes
            .iter()
            .filter(|(name, _)| name == "origin")
            .chain(remotes.iter())
            .find_map(|(_, url)| Self::from_remote_url(url))
    }

    fn graphql_url(&self) -> String {
        if self.host == "github.com" {
            "https://api.github.com/graphql".to_owned()
        } else {
            format!("https://{}/api/graphql", self.host)
        }
    }

    pub fn web_url(&self) -> String {
        format!("https://{}/{}/{}", self.host, self.owner, self.name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Checks {
    Passing,
    Failing,
    Running,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Review {
    Approved,
    ChangesRequested,
    Required,
    None,
}

#[derive(Clone, Debug)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub author_avatar: Option<String>,
    pub draft: bool,
    pub head_ref: String,
    pub base_ref: String,
    /// Full hex id of the head commit.
    pub head_oid: String,
    pub review: Review,
    pub checks: Option<Checks>,
}

/// What one refresh brings back.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub pulls: Vec<PullRequest>,
    /// Open pull requests beyond the ones listed.
    pub total: usize,
    /// Avatar URL by commit author email (lowercase).
    pub avatars: HashMap<String, String>,
}

#[derive(Debug)]
pub enum FetchError {
    /// The host refused the token; ask the user to sign in again.
    Unauthorized,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for FetchError {
    fn from(err: anyhow::Error) -> Self {
        FetchError::Other(err)
    }
}

/// A token for `host`, from the environment or the GitHub CLI. Blocks while
/// `gh` runs, so call it off the UI thread.
pub fn find_token(host: &str) -> Option<String> {
    let vars: &[&str] = if host == "github.com" {
        &["GH_TOKEN", "GITHUB_TOKEN"]
    } else {
        &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    };
    if let Some(token) = vars
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|token| !token.trim().is_empty())
    {
        return Some(token.trim().to_owned());
    }
    // An app started from Finder has a minimal PATH, so try the usual
    // install locations too.
    ["gh", "/opt/homebrew/bin/gh", "/usr/local/bin/gh"]
        .iter()
        .find_map(|gh| {
            let output = Command::new(gh)
                .args(["auth", "token", "--hostname", host])
                .output()
                .ok()?;
            let token = String::from_utf8(output.stdout).ok()?;
            (output.status.success() && !token.trim().is_empty()).then(|| token.trim().to_owned())
        })
}

const QUERY: &str = "query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: 30, orderBy: {field: UPDATED_AT, direction: DESC}) {
      totalCount
      nodes {
        number title url isDraft headRefName headRefOid baseRefName reviewDecision
        author { login avatarUrl(size: 64) }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
        baseRef { target { ... on Commit { history(first: 50) { nodes { author { email avatarUrl(size: 64) } } } } } }
      }
    }
    defaultBranchRef {
      target { ... on Commit { history(first: 100) { nodes { author { email avatarUrl(size: 64) } } } } }
    }
  }
}";

/// Open pull requests and recent author avatars of `repo`.
pub async fn fetch(
    client: Arc<dyn HttpClient>,
    repo: GitHubRepo,
    token: String,
) -> Result<Snapshot, FetchError> {
    let body = json!({
        "query": QUERY,
        "variables": { "owner": repo.owner, "name": repo.name },
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri(repo.graphql_url())
        .header("Authorization", format!("bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(AsyncBody::from(body.to_string()))
        .map_err(|err| anyhow!(err))?;
    let mut response = client.send(request).await?;
    if response.status() == StatusCode::UNAUTHORIZED {
        return Err(FetchError::Unauthorized);
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut bytes)
        .await
        .context("reading the GitHub response")?;
    if !response.status().is_success() {
        return Err(anyhow!("GitHub answered {}", response.status()).into());
    }
    let value: Value = serde_json::from_slice(&bytes).context("parsing the GitHub response")?;
    Ok(parse(&value)?)
}

/// Reads a response of [`QUERY`].
pub fn parse(value: &Value) -> Result<Snapshot> {
    if let Some(message) = value["errors"][0]["message"].as_str() {
        bail!("{message}");
    }
    let repo = &value["data"]["repository"];
    if repo.is_null() {
        bail!("the repository is not visible to this GitHub account");
    }
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let pulls: Vec<PullRequest> = repo["pullRequests"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|pr| !pr.is_null())
        .map(|pr| PullRequest {
            number: pr["number"].as_u64().unwrap_or_default(),
            title: text(&pr["title"]),
            url: text(&pr["url"]),
            // a deleted account comes back as a null author
            author: pr["author"]["login"].as_str().unwrap_or("ghost").to_owned(),
            author_avatar: pr["author"]["avatarUrl"].as_str().map(str::to_owned),
            draft: pr["isDraft"].as_bool().unwrap_or_default(),
            head_ref: text(&pr["headRefName"]),
            base_ref: text(&pr["baseRefName"]),
            head_oid: text(&pr["headRefOid"]),
            review: match pr["reviewDecision"].as_str() {
                Some("APPROVED") => Review::Approved,
                Some("CHANGES_REQUESTED") => Review::ChangesRequested,
                Some("REVIEW_REQUIRED") => Review::Required,
                _ => Review::None,
            },
            checks: match pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"].as_str()
            {
                Some("SUCCESS") => Some(Checks::Passing),
                Some("FAILURE" | "ERROR") => Some(Checks::Failing),
                Some("PENDING" | "EXPECTED") => Some(Checks::Running),
                _ => None,
            },
        })
        .collect();
    let total = repo["pullRequests"]["totalCount"]
        .as_u64()
        .map_or(pulls.len(), |n| n as usize);

    // Authors of the default branch and of every pull request's base branch,
    // where a team that merges into a release branch does its work.
    let mut avatars = HashMap::new();
    let bases = repo["pullRequests"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|pr| &pr["baseRef"]["target"]["history"]["nodes"]);
    for history in
        std::iter::once(&repo["defaultBranchRef"]["target"]["history"]["nodes"]).chain(bases)
    {
        for author in history.as_array().into_iter().flatten() {
            let author = &author["author"];
            if let (Some(email), Some(url)) =
                (author["email"].as_str(), author["avatarUrl"].as_str())
            {
                avatars
                    .entry(email.to_ascii_lowercase())
                    .or_insert_with(|| url.to_owned());
            }
        }
    }
    Ok(Snapshot {
        pulls,
        total,
        avatars,
    })
}

/// GitHub's generated picture for an account without one (an identicon):
/// always a 420×420 PNG, whatever size was asked for. At node size it reads as
/// a broken image, so such accounts get initials instead.
pub fn is_identicon(bytes: &[u8]) -> bool {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    let dimension = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    bytes.starts_with(PNG) && dimension(16) == Some(420) && dimension(20) == Some(420)
}

/// Whether each avatar URL is a real picture (`false` for an identicon). A
/// URL that cannot be read counts as real, so the image loader still tries
/// it and falls back to initials on its own.
pub async fn check_avatars(
    client: Arc<dyn HttpClient>,
    urls: Vec<String>,
) -> HashMap<String, bool> {
    let checks = urls.into_iter().map(|url| {
        let client = client.clone();
        async move {
            let identicon = async {
                let mut response = client.get(&url, AsyncBody::empty(), true).await.ok()?;
                let mut bytes = Vec::new();
                response.body_mut().read_to_end(&mut bytes).await.ok()?;
                Some(is_identicon(&bytes))
            }
            .await
            .unwrap_or(false);
            (url, !identicon)
        }
    });
    futures::future::join_all(checks)
        .await
        .into_iter()
        .collect()
}

/// GitHub's no-reply addresses name the account, so the avatar needs no
/// lookup: `123+login@users.noreply.github.com` or `login@users.noreply…`.
pub fn noreply_avatar(email: &str) -> Option<String> {
    let local = email
        .to_ascii_lowercase()
        .strip_suffix("@users.noreply.github.com")?
        .to_owned();
    let login = local
        .rsplit_once('+')
        .map_or(local.as_str(), |(_, login)| login);
    (!login.is_empty()).then(|| format!("https://avatars.githubusercontent.com/{login}?s=64"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(host: &str, owner: &str, name: &str) -> Option<GitHubRepo> {
        Some(GitHubRepo {
            host: host.into(),
            owner: owner.into(),
            name: name.into(),
        })
    }

    #[test]
    fn remote_urls() {
        let expect = repo("github.com", "kakaoent", "melon-player");
        for url in [
            "https://github.com/kakaoent/melon-player.git",
            "https://github.com/kakaoent/melon-player",
            "https://user@github.com/kakaoent/melon-player/",
            "git@github.com:kakaoent/melon-player.git",
            "ssh://git@github.com/kakaoent/melon-player.git",
            "ssh://git@github.com:22/kakaoent/melon-player",
        ] {
            assert_eq!(GitHubRepo::from_remote_url(url), expect, "{url}");
        }
        assert_eq!(
            GitHubRepo::from_remote_url("git@github.example.com:team/app.git"),
            repo("github.example.com", "team", "app")
        );
        assert_eq!(
            GitHubRepo::from_remote_url("https://gitlab.com/a/b.git"),
            None
        );
        assert_eq!(GitHubRepo::from_remote_url("/local/path/repo"), None);
        assert_eq!(
            GitHubRepo::from_remote_url("https://github.com/only-owner"),
            None
        );
    }

    #[test]
    fn origin_wins_over_other_remotes() {
        let remotes = vec![
            (
                "upstream".to_owned(),
                "https://github.com/gulbanana/gg".to_owned(),
            ),
            (
                "origin".to_owned(),
                "git@github.com:jeonsu0609/Randori.git".to_owned(),
            ),
        ];
        assert_eq!(
            GitHubRepo::pick(&remotes),
            repo("github.com", "jeonsu0609", "Randori")
        );
        let no_origin = &remotes[..1];
        assert_eq!(
            GitHubRepo::pick(no_origin),
            repo("github.com", "gulbanana", "gg")
        );
    }

    #[test]
    fn parses_pull_requests_and_avatars() {
        let value = json!({ "data": { "repository": {
            "pullRequests": { "totalCount": 3, "nodes": [
                { "number": 122, "title": "feat(ipc): limit", "url": "https://github.com/o/r/pull/122",
                  "isDraft": false, "headRefName": "feat/ipc", "headRefOid": "abc123", "baseRefName": "qa",
                  "reviewDecision": "APPROVED",
                  "author": { "login": "ian", "avatarUrl": "https://avatars/ian" },
                  "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "FAILURE" } } } ] },
                  "baseRef": { "target": { "history": { "nodes": [
                      { "author": { "email": "ian@example.com", "avatarUrl": "https://avatars/ian" } } ] } } } },
                { "number": 121, "title": "draft", "url": "u", "isDraft": true, "headRefName": "d",
                  "headRefOid": "def", "baseRefName": "qa", "reviewDecision": null, "author": null,
                  "commits": { "nodes": [ { "commit": { "statusCheckRollup": null } } ] } },
                null
            ]},
            "defaultBranchRef": { "target": { "history": { "nodes": [
                { "author": { "email": "Van@Example.com", "avatarUrl": "https://avatars/van" } },
                { "author": { "email": "van@example.com", "avatarUrl": "https://avatars/other" } }
            ]}}}
        }}});
        let snapshot = parse(&value).unwrap();
        assert_eq!(snapshot.total, 3);
        assert_eq!(snapshot.pulls.len(), 2);
        let first = &snapshot.pulls[0];
        assert_eq!(
            (first.number, first.review, first.checks),
            (122, Review::Approved, Some(Checks::Failing))
        );
        let second = &snapshot.pulls[1];
        assert_eq!(
            (second.draft, second.author.as_str(), second.checks),
            (true, "ghost", None)
        );
        // first avatar for an email wins, matched case-insensitively
        assert_eq!(snapshot.avatars["van@example.com"], "https://avatars/van");
        // base branch authors count too
        assert_eq!(snapshot.avatars["ian@example.com"], "https://avatars/ian");
    }

    #[test]
    fn graphql_errors_and_hidden_repositories_fail() {
        let error = json!({ "errors": [ { "message": "Bad credentials" } ] });
        assert_eq!(parse(&error).unwrap_err().to_string(), "Bad credentials");
        let hidden = json!({ "data": { "repository": null } });
        assert!(parse(&hidden).is_err());
    }

    #[test]
    fn identicons_are_420_pixel_pngs() {
        let png = |width: u32, height: u32| {
            let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
            bytes.extend(width.to_be_bytes());
            bytes.extend(height.to_be_bytes());
            bytes
        };
        assert!(is_identicon(&png(420, 420)));
        // a picture GitHub resized to the requested 64px
        assert!(!is_identicon(&png(64, 64)));
        assert!(!is_identicon(b"\xff\xd8\xff\xe0 JFIF"));
        assert!(!is_identicon(b""));
    }

    #[test]
    fn noreply_addresses_name_the_account() {
        assert_eq!(
            noreply_avatar("12345+jeonsu0609@users.noreply.github.com").as_deref(),
            Some("https://avatars.githubusercontent.com/jeonsu0609?s=64")
        );
        assert_eq!(
            noreply_avatar("octocat@users.noreply.github.com").as_deref(),
            Some("https://avatars.githubusercontent.com/octocat?s=64")
        );
        assert_eq!(noreply_avatar("van@kakaoent.com"), None);
    }
}
