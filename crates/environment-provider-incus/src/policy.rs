use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac as _};
use sha2::{Digest as _, Sha256};

use crate::config::BindingPolicy;

pub const META_PREFIX: &str = "user.lightspeed.";

pub fn stable_component(kind: &str, parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(kind.as_bytes());
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hex(&hash.finalize()[..10])
}

pub fn project_name(binding: &BindingPolicy) -> String {
    format!(
        "ls-{}",
        stable_component("binding", &[&binding.universe_id, &binding.binding_id])
    )
}

pub fn network_name(binding: &BindingPolicy) -> String {
    format!("{}-net", project_name(binding))
}
pub fn profile_name(binding: &BindingPolicy) -> String {
    format!("{}-vm", project_name(binding))
}
pub fn acl_name(binding: &BindingPolicy) -> String {
    format!("{}-acl", project_name(binding))
}

pub fn instance_name(
    universe_id: &str,
    binding_id: &str,
    environment_id: &str,
    incarnation_id: &str,
) -> String {
    format!(
        "ls-{}",
        stable_component(
            "instance",
            &[universe_id, binding_id, environment_id, incarnation_id]
        )
    )
}

pub fn daemon_token(
    daemon_token_key: &str,
    universe_id: &str,
    binding_id: &str,
    environment_id: &str,
    incarnation_id: &str,
    target_id: &str,
) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(daemon_token_key.as_bytes()).expect("HMAC accepts any key");
    for part in [
        universe_id,
        binding_id,
        environment_id,
        incarnation_id,
        target_id,
    ] {
        mac.update(&(part.len() as u64).to_be_bytes());
        mac.update(part.as_bytes());
    }
    format!(
        "lsp1.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 15) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_and_tokens_are_stable_and_scoped() {
        let a = instance_name("u", "b", "e", "i");
        assert_eq!(a, instance_name("u", "b", "e", "i"));
        assert_ne!(a, instance_name("u2", "b", "e", "i"));
        assert_ne!(
            daemon_token("secret", "u", "b", "e", "i", "target-a"),
            daemon_token("secret", "u", "b", "e", "i2", "target-a")
        );
        assert_ne!(
            daemon_token("secret", "u", "b", "e", "i", "target-a"),
            daemon_token("secret", "u", "b", "e", "i", "target-clone")
        );
    }
}
