//! The daemon's own identity: an Ed25519 key pair whose public half is the
//! environment identity Lightspeed binds to, and whose private half never
//! leaves the state directory.
//!
//! Whether the identity survives a restart is decided entirely by where the
//! state directory lives; the daemon has no notion of persistent or
//! ephemeral mode. Deleting the key file is the identity reset.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use environment_protocol::registration::{decode_hex, encode_hex, signed_registration_message};

/// Name of the key file inside the daemon state directory.
pub const DAEMON_KEY_FILE: &str = "daemon-key";

#[derive(Clone)]
pub struct DaemonIdentity {
    signing_key: SigningKey,
    path: PathBuf,
}

impl std::fmt::Debug for DaemonIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonIdentity")
            .field("public_key", &self.public_key_hex())
            .field("path", &self.path)
            .finish()
    }
}

impl DaemonIdentity {
    /// Load the key from `<state_dir>/daemon-key`, or generate one there.
    /// The directory is created `0700` and the file `0600`; generation is
    /// atomic so a crash cannot leave a half-written key behind.
    pub fn load_or_create(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join(DAEMON_KEY_FILE);
        if path.exists() {
            let contents = std::fs::read_to_string(&path)
                .with_context(|| format!("read daemon key {}", path.display()))?;
            let seed = decode_hex(contents.trim())
                .filter(|seed| seed.len() == 32)
                .with_context(|| {
                    format!("daemon key {} is not a 32-byte hex seed", path.display())
                })?;
            let seed: [u8; 32] = seed.try_into().expect("length checked");
            return Ok(Self {
                signing_key: SigningKey::from_bytes(&seed),
                path,
            });
        }
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("create state dir {}", state_dir.display()))?;
        restrict_dir(state_dir)?;
        let mut seed = [0u8; 32];
        rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut seed)
            .context("generate daemon identity seed")?;
        let signing_key = SigningKey::from_bytes(&seed);
        let temp = state_dir.join(format!(".{DAEMON_KEY_FILE}.{}", std::process::id()));
        write_private(&temp, &encode_hex(&signing_key.to_bytes()))?;
        if let Err(error) = std::fs::rename(&temp, &path) {
            let _ = std::fs::remove_file(&temp);
            if path.exists() {
                // Lost a race with a concurrent start of the same daemon:
                // the other key wins so both processes share one identity.
                return Self::load_or_create(state_dir);
            }
            bail!("install daemon key {}: {error}", path.display());
        }
        Ok(Self { signing_key, path })
    }

    /// Lowercase hex of the raw 32-byte public key.
    pub fn public_key_hex(&self) -> String {
        encode_hex(self.signing_key.verifying_key().as_bytes())
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Hex signature over the domain-separated challenge.
    pub fn sign_challenge(&self, nonce: &[u8]) -> String {
        encode_hex(
            &self
                .signing_key
                .sign(&signed_registration_message(nonce))
                .to_bytes(),
        )
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("create daemon key {}", path.display()))?;
    file.write_all(contents.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn restrict_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict state dir {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::Verifier as _;

    use super::*;

    #[test]
    fn identity_is_generated_once_and_reloaded_from_the_state_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let first = DaemonIdentity::load_or_create(&state).expect("create");
        let second = DaemonIdentity::load_or_create(&state).expect("reload");
        assert_eq!(first.public_key_hex(), second.public_key_hex());
        assert_eq!(first.public_key_hex().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(first.path())
                .expect("meta")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
            let dir_mode = std::fs::metadata(&state)
                .expect("meta")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
        }
        std::fs::remove_file(first.path()).expect("reset identity");
        let reset = DaemonIdentity::load_or_create(&state).expect("regenerate");
        assert_ne!(reset.public_key_hex(), first.public_key_hex());
    }

    #[test]
    fn challenge_signatures_verify_against_the_public_key_and_domain() {
        let temp = tempfile::tempdir().expect("tempdir");
        let identity = DaemonIdentity::load_or_create(temp.path()).expect("create");
        let nonce = [7u8; 32];
        let signature = decode_hex(&identity.sign_challenge(&nonce)).expect("hex");
        let signature = ed25519_dalek::Signature::from_slice(&signature).expect("signature");
        identity
            .verifying_key()
            .verify(&signed_registration_message(&nonce), &signature)
            .expect("verifies");
        assert!(
            identity.verifying_key().verify(&nonce, &signature).is_err(),
            "signature must not verify without the domain separator"
        );
        assert!(
            identity
                .verifying_key()
                .verify(&signed_registration_message(&[8u8; 32]), &signature)
                .is_err()
        );
    }

    #[test]
    fn corrupt_key_files_are_rejected_rather_than_replaced() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join(DAEMON_KEY_FILE), "not-hex").expect("write");
        assert!(DaemonIdentity::load_or_create(temp.path()).is_err());
    }
}
