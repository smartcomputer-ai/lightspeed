//! Provider-owned public hostname allocation.

use environment_protocol::routes::PublicUrl;

use crate::{Config, incus::OwnedTarget, policy};

pub fn hostname(target: &OwnedTarget, public_base_url: &str) -> anyhow::Result<String> {
    let base = reqwest::Url::parse(public_base_url)?;
    let domain = base
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("ingress publicBaseUrl has no host"))?;
    let label = policy::stable_component(
        "ingress",
        &[
            target.owner.universe_id.as_str(),
            target.owner.environment_id.as_str(),
        ],
    );
    Ok(format!("{label}.{domain}"))
}

pub fn public_endpoint(config: &Config, hostname: &str) -> anyhow::Result<PublicUrl> {
    let ingress = config
        .ingress
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("provider ingress is not configured"))?;
    let mut base = reqwest::Url::parse(&ingress.public_base_url)?;
    base.set_host(Some(hostname))
        .map_err(|_| anyhow::anyhow!("invalid ingress hostname"))?;
    Ok(PublicUrl::new(base.to_string().trim_end_matches('/'))?)
}

/// The hostname the provider allocates for the target.
pub fn allocated_hostname(config: &Config, target: &OwnedTarget) -> anyhow::Result<String> {
    let ingress = config
        .ingress
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("provider ingress is not configured"))?;
    hostname(target, &ingress.public_base_url)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::incus::InstanceStatus;

    pub(crate) fn target() -> OwnedTarget {
        let owner = policy::tests::owner("universe-a", "environment-a");
        OwnedTarget {
            target_id: policy::instance_name(&owner).try_into().unwrap(),
            owner,
            template_id: "template-a".to_owned(),
            idle: None,
            status: InstanceStatus::Running,
            ipv4_address: Some("10.0.0.2".to_owned()),
            ingress_hostname: None,
            ingress_port: None,
            location: Some("member-a".to_owned()),
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
