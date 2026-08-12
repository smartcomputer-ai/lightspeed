use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use sha2::{Digest as _, Sha256};
use tokio::fs;

use crate::config::{DaemonIdentityBinding, STATE_FILE};

const KEY_FILE: &str = "daemon-ed25519.key";

pub struct DaemonIdentity {
    key: SigningKey,
}

impl DaemonIdentity {
    pub async fn load_or_create(
        state_dir: &Path,
        binding: &DaemonIdentityBinding,
    ) -> anyhow::Result<Self> {
        fs::create_dir_all(state_dir)
            .await
            .with_context(|| format!("create envd state directory {}", state_dir.display()))?;
        protect_directory(state_dir).await?;
        bind_state_directory(state_dir, binding).await?;
        let path = state_dir.join(KEY_FILE);
        let key = match fs::read(&path).await {
            Ok(bytes) => {
                let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
                    anyhow::anyhow!("invalid envd key length in {}", path.display())
                })?;
                SigningKey::from_bytes(&bytes)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = SigningKey::generate(&mut OsRng);
                write_new_key(&path, &key.to_bytes()).await?;
                key
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        Ok(Self { key })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    pub fn daemon_id(&self) -> String {
        let digest = Sha256::digest(self.public_key());
        format!("daemon_{}", hex(&digest[..16]))
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.key.sign(message).to_bytes()
    }
}

async fn bind_state_directory(
    state_dir: &Path,
    binding: &DaemonIdentityBinding,
) -> anyhow::Result<()> {
    let path = state_dir.join(STATE_FILE);
    let expected = serde_json::to_value(binding)?;
    match fs::read(&path).await {
        Ok(bytes) => {
            let actual: serde_json::Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("decode {}", path.display()))?;
            if actual != expected {
                bail!("envd state directory is bound to a different universe/environment");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let bytes = serde_json::to_vec_pretty(&expected)?;
            write_protected_file(&path, &bytes).await?;
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    }
    Ok(())
}

async fn write_new_key(path: &PathBuf, bytes: &[u8]) -> anyhow::Result<()> {
    write_protected_file(path, bytes).await
}

async fn write_protected_file(path: &PathBuf, bytes: &[u8]) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .await
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    protect_key(path).await
}

#[cfg(unix)]
async fn protect_directory(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn protect_directory(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn protect_key(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = fs::metadata(path).await?;
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!("envd private key permissions must exclude group/other access");
    }
    Ok(())
}

#[cfg(not(unix))]
async fn protect_key(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0xf) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn key_is_stable_protected_and_bound_to_environment() {
        let temp = tempfile::tempdir().expect("tempdir");
        let binding = DaemonIdentityBinding {
            gateway_url: "https://gateway.example".to_owned(),
            universe_id: "universe-a".to_owned(),
            environment_id: "environment-a".to_owned(),
            incarnation_id: "incarnation-a".to_owned(),
        };
        let first = DaemonIdentity::load_or_create(temp.path(), &binding)
            .await
            .expect("create identity");
        let second = DaemonIdentity::load_or_create(temp.path(), &binding)
            .await
            .expect("reload identity");
        assert_eq!(first.public_key(), second.public_key());
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(temp.path().join(KEY_FILE))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o077,
            0
        );
        assert!(
            DaemonIdentity::load_or_create(
                temp.path(),
                &DaemonIdentityBinding {
                    environment_id: "environment-b".to_owned(),
                    ..binding
                },
            )
            .await
            .is_err()
        );
    }
}
