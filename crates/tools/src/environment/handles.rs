//! Compact model references; canonical IDs remain the routing and storage identity.

use engine::{BlobRef, EnvironmentId};

pub fn environment_handle(id: &str) -> String {
    if id.len() <= 24 && !id.starts_with("env:") {
        return id.to_owned();
    }
    let digest = BlobRef::from_bytes(id.as_bytes());
    let hex = id
        .strip_prefix("environment_")
        .filter(|value| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .unwrap_or_else(|| {
            digest
                .as_str()
                .strip_prefix("sha256:")
                .expect("sha256 reference")
        });
    format!("env:{}", &hex[..12])
}

pub fn environment_reference<'a>(id: &str, attached: impl IntoIterator<Item = &'a str>) -> String {
    let handle = environment_handle(id);
    if attached
        .into_iter()
        .any(|other| other != id && (other == handle || environment_handle(other) == handle))
    {
        id.to_owned()
    } else {
        handle
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvironmentReferenceError {
    #[error("unknown environment reference {0}; use an attached environment reference")]
    Unknown(String),
    #[error("ambiguous environment reference {0}; use the full environment ID")]
    Ambiguous(String),
}

pub fn resolve_environment_reference<'a>(
    reference: &str,
    attached: impl IntoIterator<Item = &'a str>,
) -> Result<EnvironmentId, EnvironmentReferenceError> {
    let mut matches = attached
        .into_iter()
        .filter(|id| *id == reference || environment_handle(id) == reference);
    let id = matches
        .next()
        .ok_or_else(|| EnvironmentReferenceError::Unknown(reference.to_owned()))?;
    if matches.any(|other| other != id) {
        return Err(EnvironmentReferenceError::Ambiguous(reference.to_owned()));
    }
    Ok(EnvironmentId::new(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_resolve_only_attached_ids_and_reject_collisions() {
        let a = "environment_9288e327bf634829b5127c7a14809ce9";
        let b = "environment_9288e327bf634829b5127c7a14809cea";
        let handle = environment_handle(a);
        assert_eq!(handle, "env:9288e327bf63");
        assert_eq!(
            resolve_environment_reference(&handle, [a])
                .unwrap()
                .as_str(),
            a
        );
        assert_eq!(
            resolve_environment_reference(a, [a, b]).unwrap().as_str(),
            a
        );
        assert!(matches!(
            resolve_environment_reference(&handle, [a, b]),
            Err(EnvironmentReferenceError::Ambiguous(_))
        ));
        assert!(matches!(
            resolve_environment_reference(&handle, ["other"]),
            Err(EnvironmentReferenceError::Unknown(_))
        ));
        assert_eq!(environment_reference(a, [a, b]), a);
        assert_eq!(environment_handle("ci"), "ci");
    }
}
