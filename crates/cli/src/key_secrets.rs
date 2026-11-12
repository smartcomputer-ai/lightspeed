//! Secrets of keys that are declared rather than minted: where a command
//! reads one from, and how one is generated without a runtime.

use std::{io::Read as _, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;

/// Where a declared key's secret comes from. It is never given on the
/// command line, where it would be visible to other processes and saved in
/// shell history.
#[derive(Args, Debug, Clone)]
#[group(required = true, multiple = false)]
pub struct SecretSourceArgs {
    /// Read the secret from this environment variable.
    #[arg(long, value_name = "VARIABLE")]
    secret_env: Option<String>,
    /// Read the secret from this file, such as a mounted secret.
    #[arg(long, value_name = "PATH")]
    secret_file: Option<PathBuf>,
    /// Read the secret from standard input.
    #[arg(long)]
    secret_stdin: bool,
}

impl SecretSourceArgs {
    pub fn read(&self) -> Result<String> {
        let secret = if let Some(variable) = &self.secret_env {
            std::env::var(variable).with_context(|| format!("{variable} is unset"))?
        } else if let Some(path) = &self.secret_file {
            std::fs::read_to_string(path)
                .with_context(|| format!("read secret file {}", path.display()))?
        } else {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            input
        };
        let secret = secret.trim();
        if secret.is_empty() {
            bail!("the supplied secret is empty");
        }
        Ok(secret.to_owned())
    }
}

/// The kinds of key whose secret can be supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    /// For `api-key put`.
    ApiKey,
    /// For `environment registration-key put`.
    RegistrationKey,
}

impl SecretKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::ApiKey => api::API_KEY_SECRET_PREFIX,
            Self::RegistrationKey => api::REGISTRATION_KEY_SECRET_PREFIX,
        }
    }
}

/// Print a new secret of `kind`. Nothing is sent to a runtime and nothing
/// is saved.
pub fn print_generated_secret(kind: SecretKind) -> Result<()> {
    println!("{}", generate_secret(kind)?);
    Ok(())
}

/// A secret in the form the runtime accepts: the kind's prefix followed by
/// 32 bytes from the operating system's random source, base64url-encoded
/// without padding.
pub fn generate_secret(kind: SecretKind) -> Result<String> {
    use base64::Engine as _;
    use rand::TryRng as _;

    let mut bytes = [0u8; api::KEY_SECRET_RANDOM_BYTES];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .context("read the operating system's random source")?;
    Ok(format!(
        "{}{}",
        kind.prefix(),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secrets_have_the_accepted_form_and_differ() {
        use base64::Engine as _;
        for (kind, prefix) in [
            (SecretKind::ApiKey, "lsk_"),
            (SecretKind::RegistrationKey, "lsrk_"),
        ] {
            let secret = generate_secret(kind).unwrap();
            let random = secret.strip_prefix(prefix).expect("prefix");
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(random)
                .expect("base64url without padding");
            assert_eq!(bytes.len(), 32);
            assert_ne!(secret, generate_secret(kind).unwrap());
        }
    }
}
