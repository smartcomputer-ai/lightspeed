//! Provider-owned public hostname allocation.

use crate::{Config, incus::OwnedTarget, policy};

pub fn hostname(target: &OwnedTarget, public_base_url: &str) -> anyhow::Result<String> {
    let base = reqwest::Url::parse(public_base_url)?;
    let domain = base
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("ingress publicBaseUrl has no host"))?;
    let label = policy::stable_component(
        "ingress",
        &[
            &target.universe_id,
            &target.binding_id,
            &target.environment_id,
        ],
    );
    Ok(format!("{label}.{domain}"))
}

pub fn public_endpoint(config: &Config, hostname: &str) -> anyhow::Result<String> {
    let ingress = config
        .ingress
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("provider ingress is not configured"))?;
    let mut base = reqwest::Url::parse(&ingress.public_base_url)?;
    base.set_host(Some(hostname))
        .map_err(|_| anyhow::anyhow!("invalid ingress hostname"))?;
    Ok(base.to_string().trim_end_matches('/').to_owned())
}

pub fn endpoint(config: &Config, target: &OwnedTarget) -> anyhow::Result<(String, String)> {
    let ingress = config
        .ingress
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("provider ingress is not configured"))?;
    let hostname = hostname(target, &ingress.public_base_url)?;
    Ok((hostname.clone(), public_endpoint(config, &hostname)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_protocol::{control::targets::HostTargetStatus, shared::HostTargetId};

    fn target() -> OwnedTarget {
        OwnedTarget {
            target_id: HostTargetId::new("target-a"),
            name: "target-a".to_owned(),
            universe_id: "universe-a".to_owned(),
            binding_id: "binding-a".to_owned(),
            environment_id: "environment-a".to_owned(),
            incarnation_id: "incarnation-a".to_owned(),
            request_id: "request-a".to_owned(),
            template_id: "template-a".to_owned(),
            image_fingerprint: "fingerprint-a".to_owned(),
            status: HostTargetStatus::Ready,
            ipv4_address: Some("10.0.0.2".to_owned()),
            ingress_hostname: None,
            ingress_port: None,
        }
    }

    #[test]
    fn hostname_is_stable_and_does_not_expose_tenant_ids() {
        let allocated = hostname(&target(), "https://env.example.com").expect("hostname");
        assert!(allocated.ends_with(".env.example.com"));
        assert!(!allocated.contains("universe-a"));
        assert_eq!(
            allocated,
            hostname(&target(), "https://env.example.com").unwrap()
        );
    }
}
