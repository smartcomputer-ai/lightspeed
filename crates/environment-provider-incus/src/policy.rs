use sha2::{Digest as _, Sha256};

use environment_protocol::control::target::{BindingRef, TargetOwner};

pub const META_PREFIX: &str = "user.lightspeed.";

/// Longest Incus instance, project or network name component the provider
/// addresses in an Incus API path.
const MAX_INCUS_NAME_BYTES: usize = 63;

pub fn stable_component(kind: &str, parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(kind.as_bytes());
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hex(&hash.finalize()[..10])
}

pub fn binding_of(owner: &TargetOwner) -> BindingRef {
    BindingRef {
        universe_id: owner.universe_id.clone(),
        binding_id: owner.binding_id.clone(),
    }
}

fn binding_component(binding: &BindingRef) -> String {
    stable_component(
        "binding",
        &[binding.universe_id.as_str(), binding.binding_id.as_str()],
    )
}

pub fn project_name(binding: &BindingRef) -> String {
    format!("ls-{}", binding_component(binding))
}

pub fn network_name(binding: &BindingRef) -> String {
    // A managed bridge's Incus network name also becomes its Linux interface
    // name, whose hard limit is 15 bytes. Keep 48 bits of the scoped digest
    // while making the resource kind visible within that limit.
    format!("ls{}n", &binding_component(binding)[..12])
}

pub fn profile_name(binding: &BindingRef) -> String {
    format!("{}-vm", project_name(binding))
}

pub fn acl_name(binding: &BindingRef) -> String {
    format!("{}-acl", project_name(binding))
}

/// The one instance name, and therefore `targetId`, an owner can hold. The
/// provider's owner index is this deterministic name inside the binding's
/// project, so create and adopt are idempotent per owner without any
/// provider-side state.
pub fn instance_name(owner: &TargetOwner) -> String {
    format!(
        "ls-{}",
        stable_component(
            "instance",
            &[
                owner.universe_id.as_str(),
                owner.binding_id.as_str(),
                owner.environment_id.as_str(),
                owner.incarnation_id.as_str(),
            ]
        )
    )
}

/// True for a string the provider may place in an Incus API path as an
/// instance name: 1 to 63 ASCII letters, digits or hyphens, not starting or
/// ending with a hyphen. Every `targetId` this provider assigns qualifies;
/// any other opaque id names nothing here.
pub fn is_instance_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_INCUS_NAME_BYTES
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
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
pub(crate) mod tests {
    use environment_protocol::ids::{BindingId, EnvironmentId, IncarnationId, UniverseId};

    use super::*;

    pub(crate) fn owner(universe: &str, binding: &str, environment: &str) -> TargetOwner {
        TargetOwner {
            universe_id: UniverseId::new(universe).unwrap(),
            binding_id: BindingId::new(binding).unwrap(),
            environment_id: EnvironmentId::new(environment).unwrap(),
            incarnation_id: IncarnationId::new("i").unwrap(),
        }
    }

    #[test]
    fn names_are_stable_and_scoped() {
        let a = instance_name(&owner("u", "b", "e"));
        assert_eq!(a, instance_name(&owner("u", "b", "e")));
        assert_ne!(a, instance_name(&owner("u2", "b", "e")));
        assert!(is_instance_name(&a));
    }

    #[test]
    fn binding_network_names_fit_the_linux_interface_limit() {
        let binding = binding_of(&owner(
            "0d3d9e5e-2428-4e60-b66e-8d4520f64e5d",
            "hz02-incus",
            "e",
        ));
        let other = binding_of(&owner(
            "61092591-cd39-4504-b845-a817e3d9cb71",
            "hz02-incus",
            "e",
        ));

        assert_eq!(network_name(&binding).len(), 15);
        assert_ne!(network_name(&binding), network_name(&other));
    }

    #[test]
    fn only_incus_names_reach_an_api_path() {
        assert!(is_instance_name("ls-0123456789abcdef0123"));
        for value in ["", "-a", "a-", "a/b", "a?b", "default/pod", &"a".repeat(64)] {
            assert!(!is_instance_name(value), "{value}");
        }
    }
}
