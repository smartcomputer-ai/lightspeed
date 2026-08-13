use sha2::{Digest as _, Sha256};

use host_protocol::control::targets::ProviderBindingContext;

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

pub fn project_name(binding: &ProviderBindingContext) -> String {
    format!(
        "ls-{}",
        stable_component("binding", &[&binding.universe_id, &binding.binding_id])
    )
}

pub fn network_name(binding: &ProviderBindingContext) -> String {
    format!("{}-net", project_name(binding))
}
pub fn profile_name(binding: &ProviderBindingContext) -> String {
    format!("{}-vm", project_name(binding))
}
pub fn acl_name(binding: &ProviderBindingContext) -> String {
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
    fn names_are_stable_and_scoped() {
        let a = instance_name("u", "b", "e", "i");
        assert_eq!(a, instance_name("u", "b", "e", "i"));
        assert_ne!(a, instance_name("u2", "b", "e", "i"));
    }
}
