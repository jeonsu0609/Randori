//! GitHub integration: open pull requests and author avatars for a
//! workspace whose remote is on GitHub (github.com or GitHub Enterprise).
//!
//! The token comes from `GH_TOKEN` / `GITHUB_TOKEN` or the GitHub CLI
//! (`gh auth token`). Randori never stores it, never logs it, and sends it
//! only to that host's GraphQL API over HTTPS.

pub mod pull;

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

    /// The REST API's URL for `path` (which starts with `/`).
    pub fn rest_url(&self, path: &str) -> String {
        if self.host == "github.com" {
            format!("https://api.github.com{path}")
        } else {
            format!("https://{}/api/v3{path}", self.host)
        }
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
    /// Full hex id of the base branch's tip, for computing the diff.
    pub base_oid: String,
    /// When GitHub last changed the pull request (ISO 8601).
    pub updated_at: String,
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
    let env = |var: &str| std::env::var(var).ok();
    if let Some(token) = env_token(host, env) {
        return Some(token);
    }
    // An app started from Finder has a minimal PATH, so try the usual
    // install locations too.
    ["gh", "/opt/homebrew/bin/gh", "/usr/local/bin/gh"]
        .iter()
        .find_map(|gh| {
            let output = gh_token_command(gh, host).output().ok()?;
            let token = String::from_utf8(output.stdout).ok()?;
            (output.status.success() && !token.trim().is_empty()).then(|| token.trim().to_owned())
        })
}

/// Environment variables holding tokens. `env_token` has already decided
/// which of them `host` may use.
const TOKEN_VARS: [&str; 4] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

/// `gh auth token` for `host`, asking only for the credentials `gh` stored
/// for that host: `gh` hands `GH_ENTERPRISE_TOKEN` to any host that is not
/// github.com, so the token variables are kept from it.
fn gh_token_command(gh: &str, host: &str) -> Command {
    let mut command = Command::new(gh);
    command.args(["auth", "token", "--hostname", host]);
    for var in TOKEN_VARS {
        command.env_remove(var);
    }
    command
}

/// The token the environment holds for `host`, with the GitHub CLI's rules:
/// `GH_TOKEN` / `GITHUB_TOKEN` are for github.com, and the enterprise ones
/// only for the host `GH_HOST` names. Any other host gets none: a remote
/// that merely looks like GitHub Enterprise must not receive a token.
fn env_token(host: &str, env: impl Fn(&str) -> Option<String>) -> Option<String> {
    let vars: &[&str] = if host == "github.com" {
        &["GH_TOKEN", "GITHUB_TOKEN"]
    } else if env("GH_HOST").is_some_and(|gh_host| gh_host.trim().eq_ignore_ascii_case(host)) {
        &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    } else {
        &[]
    };
    vars.iter()
        .filter_map(|var| env(var))
        .map(|token| token.trim().to_owned())
        .find(|token| !token.is_empty())
}

/// The pull request list. With `authors`, also the authors of the default
/// branch's recent commits, for avatars: asked on the first refresh only, as
/// the other authors are asked about by commit (`pull::fetch_commit_avatars`)
/// and the list refreshes every minute.
fn query(authors: bool) -> String {
    let history = if authors {
        "defaultBranchRef { target { ... on Commit { history(first: 100) { nodes { author { email avatarUrl(size: 64) } } } } } }"
    } else {
        ""
    };
    format!(
        "query($owner: String!, $name: String!) {{
  repository(owner: $owner, name: $name) {{
    pullRequests(states: OPEN, first: 30, orderBy: {{field: UPDATED_AT, direction: DESC}}) {{
      totalCount
      nodes {{
        number title url isDraft headRefName headRefOid baseRefName baseRefOid updatedAt reviewDecision
        author {{ login avatarUrl(size: 64) }}
        commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state }} }} }} }}
      }}
    }}
    {history}
  }}
}}"
    )
}

/// Open pull requests and recent author avatars of `repo`.
pub async fn fetch(
    client: Arc<dyn HttpClient>,
    repo: GitHubRepo,
    token: String,
    authors: bool,
) -> Result<Snapshot, FetchError> {
    let variables = json!({ "owner": repo.owner, "name": repo.name });
    let value = graphql(&client, &repo, &token, &query(authors), variables).await?;
    Ok(parse(&value)?)
}

/// Runs a GraphQL query against `repo`'s host.
pub(crate) async fn graphql(
    client: &Arc<dyn HttpClient>,
    repo: &GitHubRepo,
    token: &str,
    query: &str,
    variables: Value,
) -> Result<Value, FetchError> {
    let body = json!({ "query": query, "variables": variables });
    let bytes = send(
        client,
        Method::POST,
        &repo.graphql_url(),
        token,
        "application/json",
        AsyncBody::from(body.to_string()),
    )
    .await?;
    Ok(serde_json::from_slice(&bytes).context("parsing the GitHub response")?)
}

/// Sends one authenticated request and reads the whole answer.
pub(crate) async fn send(
    client: &Arc<dyn HttpClient>,
    method: Method,
    url: &str,
    token: &str,
    accept: &str,
    body: AsyncBody,
) -> Result<Vec<u8>, FetchError> {
    let request = Request::builder()
        .method(method)
        .uri(url)
        .header("Authorization", format!("bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", accept)
        .body(body)
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
        let detail = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| value["message"].as_str().map(str::to_owned));
        return Err(match detail {
            Some(message) => anyhow!("GitHub answered {}: {message}", response.status()),
            None => anyhow!("GitHub answered {}", response.status()),
        }
        .into());
    }
    Ok(bytes)
}

/// The node at `path` under a GraphQL answer's `data`. GitHub answers a
/// field the token may not read (a check run, say) with null and an error,
/// and the rest as usual: so errors fail the answer only when the node
/// itself is missing, and are logged otherwise.
pub(crate) fn answered<'a>(value: &'a Value, path: &[&str], missing: &str) -> Result<&'a Value> {
    let node = path.iter().fold(&value["data"], |node, key| &node[*key]);
    let messages: Vec<&str> = value["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|error| error["message"].as_str())
        .collect();
    if node.is_null() {
        match messages.first() {
            Some(message) => bail!("{message}"),
            None => bail!("{missing}"),
        }
    }
    if !messages.is_empty() {
        log::warn!(
            "GitHub left part of the answer out: {}",
            messages.join("; ")
        );
    }
    Ok(node)
}

/// Reads a response of [`query`].
pub fn parse(value: &Value) -> Result<Snapshot> {
    let repo = answered(
        value,
        &["repository"],
        "the repository is not visible to this GitHub account",
    )?;
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
            base_oid: text(&pr["baseRefOid"]),
            updated_at: text(&pr["updatedAt"]),
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

    // the default branch's recent authors, when the query asked for them
    let mut avatars = HashMap::new();
    let history = &repo["defaultBranchRef"]["target"]["history"]["nodes"];
    for author in history.as_array().into_iter().flatten() {
        let author = &author["author"];
        if let (Some(email), Some(url)) = (author["email"].as_str(), author["avatarUrl"].as_str()) {
            avatars
                .entry(email.to_ascii_lowercase())
                .or_insert_with(|| url.to_owned());
        }
    }
    Ok(Snapshot {
        pulls,
        total,
        avatars,
    })
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
                  "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "FAILURE" } } } ] } },
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
        assert_eq!(snapshot.avatars.len(), 1);
    }

    #[test]
    fn graphql_errors_and_hidden_repositories_fail() {
        let error = json!({ "errors": [ { "message": "Bad credentials" } ] });
        assert_eq!(parse(&error).unwrap_err().to_string(), "Bad credentials");
        let hidden = json!({ "data": { "repository": null } });
        assert!(parse(&hidden).is_err());
        // an unreadable field is nulled with an error; the list still loads
        let partial = json!({
            "data": { "repository": { "pullRequests": { "totalCount": 1, "nodes": [
                { "number": 5, "title": "T", "commits": { "nodes": [ { "commit": null } ] } }
            ] } } },
            "errors": [ { "message": "Resource not accessible by integration" } ]
        });
        let snapshot = parse(&partial).expect("the list");
        assert_eq!(snapshot.pulls.len(), 1);
        assert_eq!(snapshot.pulls[0].checks, None);
    }

    #[test]
    fn only_the_first_list_query_asks_for_authors() {
        assert!(query(true).contains("defaultBranchRef"));
        assert!(!query(false).contains("defaultBranchRef"));
        // the commits a pull request's diff runs between, in both
        for authors in [true, false] {
            assert!(query(authors).contains("baseRefOid"));
        }
    }

    #[test]
    fn enterprise_tokens_go_only_to_gh_host() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(var, _)| *var == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        let vars: &[(&str, &str)] = &[
            ("GH_TOKEN", "public"),
            ("GH_ENTERPRISE_TOKEN", "corp"),
            ("GH_HOST", "github.corp.example"),
        ];
        assert_eq!(
            env_token("github.com", env(vars)).as_deref(),
            Some("public")
        );
        assert_eq!(
            env_token("github.corp.example", env(vars)).as_deref(),
            Some("corp")
        );
        // a remote that only looks like GitHub Enterprise gets nothing
        assert_eq!(env_token("github.evil.example", env(vars)), None);
        // without GH_HOST the enterprise token is never used
        let no_host: &[(&str, &str)] = &[("GH_ENTERPRISE_TOKEN", "corp")];
        assert_eq!(env_token("github.corp.example", env(no_host)), None);
    }

    #[test]
    fn gh_never_sees_the_token_variables() {
        let command = gh_token_command("gh", "github.evil.example");
        let removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        for var in TOKEN_VARS {
            assert!(removed.iter().any(|name| name == var), "{var} reaches gh");
        }
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["auth", "token", "--hostname", "github.evil.example"]);
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
