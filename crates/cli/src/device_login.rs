//! Browser approval for a CLI-owned Platform session. The device code and session
//! token stay in request bodies/headers and the private credential file.
use crate::{
    api_client::HttpAgentApi,
    connection::{self, CredentialKind, ResolvedConnection, SavedConnection, ServerKind},
};
use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;
use std::{
    process::{Command, Stdio},
    time::Duration,
};

const CLIENT_ID: &str = "lightspeed-cli";

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Deployment origin or API URL, including any deployment base path.
    #[arg(long)]
    pub url: Option<String>,
    /// Save and select this named connection.
    #[arg(long)]
    pub name: Option<String>,
    /// Print the approval URL without opening a browser.
    #[arg(long)]
    pub no_browser: bool,
    /// Permit plain HTTP on a trusted development network.
    #[arg(long)]
    pub allow_insecure_http: bool,
}
#[derive(Debug, Args)]
pub struct LogoutArgs {
    /// Log out this connection rather than the selected connection.
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: Option<u64>,
}
#[derive(Deserialize)]
struct DeviceToken {
    access_token: String,
    token_type: String,
    expires_in: u64,
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .context("create device login client")
}

fn verification_url(endpoint: &str, code: &DeviceCode) -> Result<reqwest::Url> {
    let deployment = reqwest::Url::parse(endpoint)?;
    // Validate both values, even when the browser uses the complete form.
    for value in [
        Some(code.verification_uri.as_str()),
        code.verification_uri_complete.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let url = reqwest::Url::parse(value).context("invalid device approval URL")?;
        if url.origin() != deployment.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            bail!("device approval URL must use the deployment's origin");
        }
        if value.contains(&code.device_code) {
            bail!("device approval URL must not expose the device credential");
        }
    }
    reqwest::Url::parse(
        code.verification_uri_complete
            .as_deref()
            .unwrap_or(&code.verification_uri),
    )
    .context("invalid device approval URL")
}

fn open_browser(url: &reqwest::Url) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    command
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("open approval page")?;
    Ok(())
}

async fn obtain_token(endpoint: &str, no_browser: bool) -> Result<String> {
    let client = http_client()?;
    let response = client
        .post(connection::auth_url(endpoint, "device/code")?)
        .json(&serde_json::json!({"client_id": CLIENT_ID}))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("device login endpoint is unavailable"))?;
    if !response.status().is_success() {
        bail!(
            "device login was refused (HTTP {})",
            response.status().as_u16()
        );
    }
    let code: DeviceCode = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("invalid device login response"))?;
    if code.device_code.is_empty()
        || code.device_code.len() > 4096
        || code.user_code.is_empty()
        || code.user_code.len() > 64
        || code.user_code.chars().any(char::is_control)
        || code.expires_in == 0
    {
        bail!("invalid device login response");
    }
    let approval = verification_url(endpoint, &code)?;
    let deadline = tokio::time::Instant::now()
        .checked_add(Duration::from_secs(code.expires_in))
        .context("invalid device login expiry")?;
    println!(
        "Open {approval}\nEnter code: {}\nWaiting for browser approval…",
        code.user_code
    );
    if !no_browser && open_browser(&approval).is_err() {
        eprintln!("Open the URL above in your browser to continue.");
    }
    let mut interval = Duration::from_secs(code.interval.unwrap_or(5).max(1));
    let token_url = connection::auth_url(endpoint, "device/token")?;
    loop {
        let next = tokio::time::Instant::now()
            .checked_add(interval)
            .unwrap_or(deadline)
            .min(deadline);
        tokio::select! {
            _ = tokio::time::sleep_until(next) => {},
            _ = tokio::signal::ctrl_c() => bail!("login cancelled"),
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("device code expired; run connect login again");
        }
        let request = client
            .post(token_url.clone())
            .json(&serde_json::json!({
                "client_id": CLIENT_ID,
                "device_code": code.device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            }))
            .send();
        let response = tokio::select! {
            result = tokio::time::timeout_at(deadline, request) => result.context("device code expired; run connect login again")?.map_err(|_| anyhow::anyhow!("device authorization polling failed"))?,
            _ = tokio::signal::ctrl_c() => bail!("login cancelled"),
        };
        if response.status().is_redirection() {
            bail!("device authorization must not redirect");
        }
        let status = response.status();
        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("invalid device authorization response"))?;
        if status.is_success() {
            let token: DeviceToken = serde_json::from_value(value)
                .map_err(|_| anyhow::anyhow!("invalid device session response"))?;
            if !token.token_type.eq_ignore_ascii_case("Bearer")
                || token.expires_in == 0
                || token.access_token.is_empty()
                || token.access_token.len() > 4096
                || token.access_token.chars().any(char::is_whitespace)
            {
                bail!("invalid device session response");
            }
            return Ok(token.access_token);
        }
        match value.get("error").and_then(serde_json::Value::as_str) {
            Some("authorization_pending") => {}
            Some("slow_down") => interval = interval.saturating_add(Duration::from_secs(5)),
            Some("access_denied" | "authorization_declined") => {
                bail!("browser authorization was denied")
            }
            Some("expired_token" | "invalid_grant") => {
                bail!("device code expired or was already used; run connect login again")
            }
            _ => bail!("device authorization failed (HTTP {})", status.as_u16()),
        }
    }
}

pub async fn login(args: LoginArgs, selected: Option<&str>, api_url: Option<&str>) -> Result<()> {
    let dir = connection::config_dir()?;
    let mut config = connection::load_at(&dir)?;
    let explicit_url = args.url.as_deref().or(api_url);
    let selected_name = args.name.as_deref().or(selected).or_else(|| {
        if explicit_url.is_none() {
            config.selected.as_deref()
        } else {
            None
        }
    });
    let previous = selected_name
        .and_then(|name| config.connections.get(name))
        .cloned();
    let url = explicit_url
        .or_else(|| previous.as_ref().map(|saved| saved.endpoint.as_str()))
        .context("supply a deployment URL or select a saved connection")?;
    let allow_insecure_http = args.allow_insecure_http
        || (explicit_url.is_none()
            && previous
                .as_ref()
                .is_some_and(|saved| saved.allow_insecure_http));
    let endpoint = connection::endpoint_with_policy(url, allow_insecure_http)?;
    let name = selected_name.map(str::to_owned).unwrap_or_else(|| {
        reqwest::Url::parse(&endpoint)
            .unwrap()
            .host_str()
            .unwrap_or("platform")
            .to_owned()
    });
    if name.trim().is_empty() {
        bail!("connection name must not be empty");
    }
    if let Some(existing) = config.connections.get(&name)
        && existing.endpoint != endpoint
    {
        bail!("connection name already belongs to another deployment; choose --name");
    }
    let previous = config.connections.get(&name).cloned();
    let secret = obtain_token(&endpoint, args.no_browser).await?;
    let file = dir
        .join("credentials")
        .join(uuid::Uuid::new_v4().to_string());
    let candidate = ResolvedConnection {
        name: Some(name.clone()),
        endpoint: endpoint.clone(),
        secret: Some(secret),
        credential_kind: CredentialKind::PlatformSession,
        credential_file: Some(file.clone()),
        owned_credential: true,
        server_kind: Some(ServerKind::Platform),
        account_id: previous.as_ref().and_then(|saved| saved.account_id.clone()),
        universe: previous.as_ref().and_then(|saved| saved.universe.clone()),
        allow_insecure_http,
        ..Default::default()
    };
    if let Err(error) =
        connection::private_write(&file, candidate.secret.as_ref().unwrap().as_bytes())
    {
        let mut unsaved = candidate.clone();
        unsaved.credential_file = None;
        unsaved.owned_credential = false;
        best_effort_revoke(unsaved).await;
        return Err(error);
    }
    let verified = match connection::verify(candidate.clone()).await {
        Ok(value) => value,
        Err(error) => {
            best_effort_revoke(candidate).await;
            let _ = std::fs::remove_file(&file);
            return Err(error.context("verify approved Platform session"));
        }
    };
    config.connections.insert(
        name.clone(),
        SavedConnection {
            endpoint,
            single: false,
            credential_kind: CredentialKind::PlatformSession,
            credential_file: Some(file.clone()),
            owned_credential: true,
            server_kind: Some(ServerKind::Platform),
            account_id: verified.account_id,
            universe: verified.universe,
            allow_insecure_http,
        },
    );
    config.selected = Some(name.clone());
    if let Err(error) = connection::save_at(&dir, &config) {
        best_effort_revoke(candidate).await;
        let _ = std::fs::remove_file(file);
        return Err(error);
    }
    if let Some(previous) = previous {
        if previous.credential_kind == CredentialKind::PlatformSession {
            match connection::saved_resolved(&name, &previous) {
                Ok(active) => best_effort_revoke(active).await,
                Err(_) => eprintln!(
                    "The previous CLI session could not be revoked; revoke it in Platform if it is still active."
                ),
            }
        }
        connection::remove_owned_secret(&dir, &previous)?;
    }
    println!(
        "Signed in to {name} as {}. Selected this deployment.",
        verified
            .principal
            .as_ref()
            .map(|p| p.name.as_str())
            .unwrap_or("user")
    );
    Ok(())
}

async fn revoke_session(active: ResolvedConnection) -> Result<()> {
    let client = HttpAgentApi::with_connection(active);
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        client.call_platform("platform/session/revoke", serde_json::json!({})),
    )
    .await
    .context("session revocation timed out; retry logout")?;
    match result {
        Ok(_) => Ok(()),
        Err(error) if error.kind == api::AgentApiErrorKind::Unauthenticated => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn best_effort_revoke(active: ResolvedConnection) {
    if revoke_session(active).await.is_err() {
        eprintln!(
            "A CLI session could not be revoked automatically; revoke it in Platform if it is still active."
        );
    }
}

pub async fn logout(args: LogoutArgs, selected: Option<&str>) -> Result<()> {
    let dir = connection::config_dir()?;
    let mut config = connection::load_at(&dir)?;
    let name = args
        .name
        .as_deref()
        .or(selected)
        .or(config.selected.as_deref())
        .context("no selected connection")?
        .to_owned();
    let saved = config
        .connections
        .get(&name)
        .context("unknown connection")?
        .clone();
    if saved.credential_kind != CredentialKind::PlatformSession {
        bail!(
            "this connection has no CLI login session; API keys are managed separately, or use `connect remove` to forget a connection"
        );
    }
    let active = connection::saved_resolved(&name, &saved)?;
    revoke_session(active).await?;
    let local = config.connections.get_mut(&name).unwrap();
    local.credential_file = None;
    local.owned_credential = false;
    local.credential_kind = CredentialKind::None;
    connection::save_at(&dir, &config)?;
    connection::remove_owned_secret(&dir, &saved)?;
    println!("Logged out of {name}. Only this CLI session was revoked.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn code(url: &str) -> DeviceCode {
        DeviceCode {
            device_code: "private-device-code".into(),
            user_code: "ABCD-EFGH".into(),
            verification_uri: url.into(),
            verification_uri_complete: None,
            expires_in: 300,
            interval: Some(5),
        }
    }
    #[test]
    fn approval_stays_on_the_deployment_origin_and_never_exposes_the_device_secret() {
        assert!(
            verification_url(
                "https://app.example/team/api/v1",
                &code("https://app.example/team/device?user_code=ABCD-EFGH")
            )
            .is_ok()
        );
        for url in [
            "https://other.example/device",
            "http://app.example/device",
            "https://app.example/device?device_code=private-device-code",
            "https://user@app.example/device",
        ] {
            assert!(verification_url("https://app.example/api/v1", &code(url)).is_err());
        }
    }
}
