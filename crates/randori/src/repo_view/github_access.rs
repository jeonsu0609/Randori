//! Getting at GitHub: the token the requests carry and a time limit on each,
//! for the pull request section and the pull request view alike.

use std::time::Duration;

use crate::github::{self, FetchError};

/// The section's token, or one looked up now (the environment, then the
/// GitHub CLI) off the UI thread.
pub(super) fn github_token(
    host: String,
    cached: Option<String>,
    executor: gpui_kit::BackgroundExecutor,
) -> futures::future::BoxFuture<'static, Option<String>> {
    use futures::FutureExt as _;
    async move {
        match cached {
            Some(token) => Some(token),
            None => {
                executor
                    .spawn(async move { github::find_token(&host) })
                    .await
            }
        }
    }
    .boxed()
}

/// How long a GitHub request may go without an answer.
const GITHUB_TIMEOUT: Duration = Duration::from_secs(30);

/// `request`, or an error once [`GITHUB_TIMEOUT`] passes without its answer:
/// a stalled connection (a network change, waking from sleep) would otherwise
/// keep the list fetching, and the pull request view loading, for good.
pub(super) async fn within<T>(
    executor: gpui_kit::BackgroundExecutor,
    request: impl std::future::Future<Output = Result<T, FetchError>>,
) -> Result<T, FetchError> {
    use futures::future::{Either, select};
    let timer = executor.timer(GITHUB_TIMEOUT);
    let request = std::pin::pin!(request);
    match select(request, timer).await {
        Either::Left((answer, _)) => answer,
        Either::Right(_) => Err(FetchError::Other(anyhow::anyhow!(
            "GitHub did not answer within {} seconds",
            GITHUB_TIMEOUT.as_secs()
        ))),
    }
}
