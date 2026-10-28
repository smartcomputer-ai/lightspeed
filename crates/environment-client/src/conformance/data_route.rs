//! Data-route refusal conformance: a provider data route that does not
//! upgrade answers a typed [`DataRouteRefusal`] body with the status that
//! refusal names, a 503 carries `Retry-After`, and the route answers within
//! [`MAX_DATA_ROUTE_HOLD_MS`]. [`await_upgrade`] dials until the route
//! upgrades, waiting out each conforming 503 at its `Retry-After`; the
//! [`controller`](super::controller) suite runs both against a created
//! target when it is given the provider's data route base.
//!
//! ```no_run
//! # async fn example(options: environment_client::ConnectOptions) {
//! use environment_client::conformance::data_route::expect_refusal;
//!
//! // A data route of a target the provider was just told to pause.
//! let refusal = expect_refusal("wss://provider.example/routes/u1/env_1/ls-3f2a", options)
//!     .await
//!     .expect("the route refuses with a typed body");
//! # let _ = refusal;
//! # }
//! ```

use std::time::{Duration, Instant};

use environment_protocol::{limits::MAX_DATA_ROUTE_HOLD_MS, routes::DataRouteRefusal};

use crate::{ClientError, ConnectOptions, DataClient};

/// Slack on top of [`MAX_DATA_ROUTE_HOLD_MS`] for the dial itself.
const HOLD_SLACK: Duration = Duration::from_secs(2);

/// Check one refused upgrade: it is typed as a [`DataRouteRefusal`], its
/// status is the one the refusal names (503, 410 or 409), and a 503
/// carries `Retry-After` of at least one second. The client types a
/// refusal from its status when the body was not read with the headers
/// ([`ClientError::UpgradeRejected`]), so this check cannot tell a missing
/// body from one the transport split off.
pub fn check_refusal(error: &ClientError) -> Result<&DataRouteRefusal, String> {
    let ClientError::UpgradeRejected {
        status,
        refusal,
        retry_after_ms,
    } = error
    else {
        return Err(format!("expected a refused upgrade, got {error}"));
    };
    let refusal = refusal
        .as_ref()
        .ok_or_else(|| format!("HTTP {status} carries no data route refusal body"))?;
    if *status != refusal.status() {
        return Err(format!(
            "refusal {} answered with HTTP {status}, expected {}",
            refusal.state(),
            refusal.status()
        ));
    }
    if refusal.is_retryable() && !retry_after_ms.is_some_and(|ms| ms >= 1_000) {
        return Err(format!(
            "refusal {} carries no Retry-After of at least 1 s",
            refusal.state()
        ));
    }
    Ok(refusal)
}

/// Dial `url` as a data route and require a conforming refusal within
/// [`MAX_DATA_ROUTE_HOLD_MS`] (plus dial slack). Returns the refusal.
pub async fn expect_refusal(
    url: &str,
    options: ConnectOptions,
) -> Result<DataRouteRefusal, String> {
    let started = Instant::now();
    let outcome = DataClient::connect_initialized(url, options).await;
    let elapsed = started.elapsed();
    let error = match outcome {
        Ok((client, _)) => {
            let _ = client.close().await;
            return Err("the data route upgraded instead of refusing".to_owned());
        }
        Err(error) => error,
    };
    let refusal = check_refusal(&error)?.clone();
    check_hold(elapsed)?;
    Ok(refusal)
}

/// Dial `url` as a data route until it upgrades, within `timeout`. Every
/// refusal on the way must conform ([`check_refusal`]), be a 503 and answer
/// within [`MAX_DATA_ROUTE_HOLD_MS`]; the next dial follows its
/// `Retry-After`. Returns the 503 refusals seen, in order: `waking` for a
/// powered-down target the dial woke, `booting` while envd starts.
pub async fn await_upgrade(
    url: &str,
    options: ConnectOptions,
    timeout: Duration,
) -> Result<Vec<DataRouteRefusal>, String> {
    let deadline = Instant::now() + timeout;
    let mut refusals = Vec::new();
    loop {
        let started = Instant::now();
        let outcome = DataClient::connect_initialized(url, options.clone()).await;
        let elapsed = started.elapsed();
        let error = match outcome {
            Ok((client, _)) => {
                let _ = client.close().await;
                return Ok(refusals);
            }
            Err(error) => error,
        };
        let refusal = check_refusal(&error)?.clone();
        check_hold(elapsed)?;
        if !refusal.is_retryable() {
            return Err(format!(
                "the data route refused {refusal} instead of upgrading"
            ));
        }
        let retry_after = Duration::from_millis(error.retry_after_ms().unwrap_or(1_000));
        if Instant::now() + retry_after > deadline {
            return Err(format!(
                "the data route did not upgrade within {timeout:?}; last refusal {refusal}"
            ));
        }
        refusals.push(refusal);
        tokio::time::sleep(retry_after).await;
    }
}

/// A dial that refused must answer within [`MAX_DATA_ROUTE_HOLD_MS`] plus
/// [`HOLD_SLACK`].
fn check_hold(elapsed: Duration) -> Result<(), String> {
    let bound = Duration::from_millis(MAX_DATA_ROUTE_HOLD_MS) + HOLD_SLACK;
    if elapsed > bound {
        return Err(format!(
            "the data route held the dial for {elapsed:?}, over the {bound:?} bound"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use environment_protocol::metadata::DescriptiveText;

    use super::*;

    fn rejected(
        status: u16,
        refusal: Option<DataRouteRefusal>,
        retry_after_ms: Option<u64>,
    ) -> ClientError {
        ClientError::UpgradeRejected {
            status,
            refusal,
            retry_after_ms,
        }
    }

    #[test]
    fn refusals_must_match_their_status_and_carry_retry_after() {
        assert_eq!(
            check_refusal(&rejected(
                503,
                Some(DataRouteRefusal::Waking {}),
                Some(2_000)
            )),
            Ok(&DataRouteRefusal::Waking {})
        );
        assert!(check_refusal(&rejected(503, Some(DataRouteRefusal::Waking {}), None)).is_err());
        assert!(check_refusal(&rejected(503, Some(DataRouteRefusal::Gone {}), None)).is_err());
        assert!(check_refusal(&rejected(410, None, None)).is_err());
        assert!(check_refusal(&ClientError::Transport("x".to_owned())).is_err());
        let failed = DataRouteRefusal::Failed {
            message: DescriptiveText::lossy("disk lost").unwrap(),
        };
        assert_eq!(
            check_refusal(&rejected(409, Some(failed.clone()), None)),
            Ok(&failed)
        );
    }
}
