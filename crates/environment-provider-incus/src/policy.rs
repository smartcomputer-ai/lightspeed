use sha2::{Digest as _, Sha256};

use environment_protocol::{control::target::TargetOwner, ids::UniverseId};

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

/// The isolation unit of every Incus resource the provider creates for a
/// universe: its project, network, ACL and profile all derive from it.
fn universe_component(universe_id: &UniverseId) -> String {
    stable_component("universe", &[universe_id.as_str()])
}

pub fn project_name(universe_id: &UniverseId) -> String {
    format!("ls-{}", universe_component(universe_id))
}

pub fn network_name(universe_id: &UniverseId) -> String {
    // A managed bridge's Incus network name also becomes its Linux interface
    // name, whose hard limit is 15 bytes. Keep 48 bits of the scoped digest
    // while making the resource kind visible within that limit.
    format!("ls{}n", &universe_component(universe_id)[..12])
}

pub fn profile_name(universe_id: &UniverseId) -> String {
    format!("{}-vm", project_name(universe_id))
}

pub fn acl_name(universe_id: &UniverseId) -> String {
    format!("{}-acl", project_name(universe_id))
}

/// The one instance name, and therefore `targetId`, an owner can hold. The
/// provider's owner index is this deterministic name inside the universe's
/// project, so create and close are idempotent per owner without any
/// provider-side state.
pub fn instance_name(owner: &TargetOwner) -> String {
    format!(
        "ls-{}",
        stable_component(
            "instance",
            &[owner.universe_id.as_str(), owner.environment_id.as_str()]
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
    use environment_protocol::ids::EnvironmentId;

    use super::*;

    pub(crate) fn owner(universe: &str, environment: &str) -> TargetOwner {
        TargetOwner {
            universe_id: UniverseId::new(universe).unwrap(),
            environment_id: EnvironmentId::new(environment).unwrap(),
        }
    }

    #[test]
    fn names_are_stable_and_scoped() {
        let a = instance_name(&owner("u", "e"));
        assert_eq!(a, instance_name(&owner("u", "e")));
        assert_ne!(a, instance_name(&owner("u2", "e")));
        assert_ne!(a, instance_name(&owner("u", "e2")));
        assert!(is_instance_name(&a));
    }

    #[test]
    fn universe_resources_share_one_isolation_unit() {
        let universe = UniverseId::new("0d3d9e5e-2428-4e60-b66e-8d4520f64e5d").unwrap();
        let other = UniverseId::new("61092591-cd39-4504-b845-a817e3d9cb71").unwrap();

        assert_eq!(network_name(&universe).len(), 15);
        assert_ne!(network_name(&universe), network_name(&other));
        assert_ne!(project_name(&universe), project_name(&other));
        assert!(profile_name(&universe).starts_with(&project_name(&universe)));
        assert!(acl_name(&universe).starts_with(&project_name(&universe)));
        // The instance name never exposes the universe it belongs to.
        assert!(!instance_name(&owner(universe.as_str(), "e")).contains(universe.as_str()));
    }

    #[test]
    fn only_incus_names_reach_an_api_path() {
        assert!(is_instance_name("ls-0123456789abcdef0123"));
        for value in ["", "-a", "a-", "a/b", "a?b", "default/pod", &"a".repeat(64)] {
            assert!(!is_instance_name(value), "{value}");
        }
    }
}
