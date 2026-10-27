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
use environment_protocol::{
    bytes::{decode_hex_array, encode_hex},
    registration::{
        ChallengeNonce, DaemonPublicKey, Ed25519Signature, signed_registration_message,
    },
    routes::CanonicalOrigin,
};

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
            .field("public_key", &self.public_key())
            .field("path", &self.path)
            .finish()
    }
}

impl DaemonIdentity {
    /// Load the key from `<state_dir>/daemon-key`, or generate one there.
    /// The file is created `0600` and installed without ever replacing an
    /// existing key: when two first starts race, the key installed first
    /// wins and both processes load it.
    pub fn load_or_create(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join(DAEMON_KEY_FILE);
        if let Some(identity) = Self::load(&path)? {
            return Ok(identity);
        }
        let mut seed = [0u8; 32];
        rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut seed)
            .context("generate daemon identity seed")?;
        let signing_key = SigningKey::from_bytes(&seed);
        let mut suffix = [0u8; 8];
        rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut suffix)
            .context("generate temporary key name")?;
        let temp = state_dir.join(format!(".{DAEMON_KEY_FILE}.{}", encode_hex(&suffix)));
        write_private(&temp, &encode_hex(&signing_key.to_bytes()))?;
        // A hard link fails when the destination exists, unlike rename,
        // which would silently replace a key a concurrent start installed.
        let installed = std::fs::hard_link(&temp, &path);
        let _ = std::fs::remove_file(&temp);
        match installed {
            Ok(()) => {
                sync_dir(state_dir);
                Ok(Self { signing_key, path })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Self::load(&path)?
                .with_context(|| format!("daemon key {} vanished during install", path.display())),
            Err(error) => bail!("install daemon key {}: {error}", path.display()),
        }
    }

    fn load(path: &Path) -> Result<Option<Self>> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("read daemon key {}", path.display()));
            }
        };
        let seed = decode_hex_array::<32>(contents.trim())
            .with_context(|| format!("daemon key {} is not a 32-byte hex seed", path.display()))?;
        Ok(Some(Self {
            signing_key: SigningKey::from_bytes(&seed),
            path: path.to_path_buf(),
        }))
    }

    /// `daemonPublicKey`.
    pub fn public_key(&self) -> DaemonPublicKey {
        DaemonPublicKey::from_bytes(self.signing_key.verifying_key().to_bytes())
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// `signature` over the challenge nonce bound to the canonical origin of
    /// the gateway URL this daemon dialed.
    pub fn sign_challenge(
        &self,
        nonce: &ChallengeNonce,
        origin: &CanonicalOrigin,
    ) -> Ed25519Signature {
        Ed25519Signature::from_bytes(
            self.signing_key
                .sign(&signed_registration_message(nonce, origin))
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

/// Make the new directory entry durable; best effort.
fn sync_dir(path: &Path) {
    if let Ok(directory) = std::fs::File::open(path) {
        let _ = directory.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::Verifier as _;
    use environment_protocol::routes::canonical_origin;

    use super::*;

    #[test]
    fn identity_is_generated_once_and_reloaded_from_the_state_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path();
        let first = DaemonIdentity::load_or_create(state).expect("create");
        let second = DaemonIdentity::load_or_create(state).expect("reload");
        assert_eq!(first.public_key(), second.public_key());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(first.path())
                .expect("meta")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(
            std::fs::read_dir(state).unwrap().count(),
            1,
            "no temporary file is left behind"
        );
        std::fs::remove_file(first.path()).expect("reset identity");
        let reset = DaemonIdentity::load_or_create(state).expect("regenerate");
        assert_ne!(reset.public_key(), first.public_key());
    }

    #[test]
    fn concurrent_first_starts_share_the_key_installed_first() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let state = state.clone();
                std::thread::spawn(move || DaemonIdentity::load_or_create(&state).unwrap())
            })
            .collect();
        let keys: std::collections::BTreeSet<String> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().public_key().to_hex())
            .collect();
        assert_eq!(keys.len(), 1, "every racing start holds the installed key");
        let on_disk = DaemonIdentity::load_or_create(&state).unwrap();
        assert_eq!(
            keys.into_iter().next().unwrap(),
            on_disk.public_key().to_hex()
        );
    }

    #[test]
    fn challenge_signatures_bind_the_nonce_and_the_dialed_origin() {
        let temp = tempfile::tempdir().expect("tempdir");
        let identity = DaemonIdentity::load_or_create(temp.path()).expect("create");
        let nonce = ChallengeNonce::from_bytes([7u8; 32]);
        let origin = canonical_origin("wss://gateway.example/environment-gateway/connect").unwrap();
        let signature = identity.sign_challenge(&nonce, &origin);
        let signature = ed25519_dalek::Signature::from_bytes(signature.as_bytes());
        let verifying = identity.verifying_key();
        verifying
            .verify_strict(&signed_registration_message(&nonce, &origin), &signature)
            .expect("verifies");
        assert!(verifying.verify(nonce.as_bytes(), &signature).is_err());
        let elsewhere = canonical_origin("wss://other.example").unwrap();
        assert!(
            verifying
                .verify(&signed_registration_message(&nonce, &elsewhere), &signature)
                .is_err(),
            "a signature for one gateway does not verify at another"
        );
        let other_nonce = ChallengeNonce::from_bytes([8u8; 32]);
        assert!(
            verifying
                .verify(
                    &signed_registration_message(&other_nonce, &origin),
                    &signature
                )
                .is_err()
        );
    }

    #[test]
    fn corrupt_key_files_are_rejected_rather_than_replaced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join(DAEMON_KEY_FILE);
        std::fs::write(&path, "not-hex").expect("write");
        assert!(DaemonIdentity::load_or_create(temp.path()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not-hex");
    }
}
