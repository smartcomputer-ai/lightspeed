//! Connection presentation captured once for a chat, without polling on redraw.
use crate::connection::{ResolvedConnection, UniverseStatus};

#[derive(Default)]
pub(super) struct ConnectionDisplay {
    pub footer: Option<String>,
    pub details: String,
    pub header_context: Vec<(String, String)>,
}

impl ConnectionDisplay {
    pub async fn load() -> anyhow::Result<Self> {
        let Some(connection) = crate::connection::ACTIVE.get() else {
            return Ok(Self {
                footer: None,
                details: "Connection details unavailable.".into(),
                header_context: Vec::new(),
            });
        };
        let universe = crate::connection::universe_status(connection).await?;
        Ok(Self::new(connection, &universe))
    }

    fn new(connection: &ResolvedConnection, universe: &UniverseStatus) -> Self {
        let url = reqwest::Url::parse(&connection.endpoint).ok();
        let generated_dev = connection
            .name
            .as_deref()
            .and_then(|name| name.strip_prefix("dev-"))
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .is_some_and(|id| id.get_version_num() == 5);
        let loopback = url
            .as_ref()
            .and_then(|url| url.host_str())
            .is_some_and(|host| {
                host == "localhost"
                    || host
                        .trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
        let endpoint_label = url.as_ref().map_or_else(
            || connection.endpoint.clone(),
            |url| {
                format!(
                    "{}{}",
                    url.host_str().unwrap_or("runtime"),
                    url.port()
                        .map(|port| format!(":{port}"))
                        .unwrap_or_default()
                )
            },
        );
        let name = connection
            .name
            .as_deref()
            .filter(|_| !generated_dev)
            .unwrap_or(&endpoint_label);
        let universe_label = universe.handle.clone().unwrap_or_else(|| {
            universe
                .universe_id
                .as_deref()
                .map(|id| id.chars().take(8).collect())
                .unwrap_or_else(|| "unselected".into())
        });
        let footer = (!(generated_dev && loopback)).then(|| format!("{name} / {universe_label}"));
        let runtime = if generated_dev && loopback {
            format!("local development · {}", connection.endpoint)
        } else if connection.name.is_some() && !generated_dev {
            format!("{name} · {}", connection.endpoint)
        } else {
            connection.endpoint.clone()
        };
        let mut header_universe = universe
            .handle
            .as_deref()
            .or(universe.universe_id.as_deref())
            .unwrap_or("none selected")
            .to_owned();
        if let Some(caller) = &connection.caller {
            header_universe.push_str(if caller.single {
                " · single mode"
            } else {
                " · runtime token"
            });
        }
        let header_context = vec![
            ("Universe".into(), header_universe),
            ("Runtime".into(), runtime),
        ];

        let mut details = format!(
            "Connection: {}\n  Endpoint: {}\n",
            connection.name.as_deref().unwrap_or("environment"),
            connection.endpoint,
        );
        if let Some(caller) = &connection.caller {
            details.push_str(&format!(
                "  Mode: {}\n  Token: {}\n  Authority: full runtime access\n",
                if caller.single {
                    "single (no token)"
                } else {
                    "authenticated"
                },
                caller.token_name.as_deref().unwrap_or("none"),
            ));
        }
        details.push_str(&format!(
            "\nUniverse:\n  Handle: {}\n  UUID: {}",
            universe.handle.as_deref().unwrap_or("unavailable"),
            universe
                .universe_id
                .as_deref()
                .unwrap_or(if universe.handle.is_some() {
                    "unavailable"
                } else {
                    "none selected"
                }),
        ));
        if let Some(reason) = &universe.unavailable_reason {
            details.push_str(&format!("\n  Lookup: {reason}"));
        }
        Self {
            footer,
            details,
            header_context,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(name: Option<&str>, endpoint: &str) -> ResolvedConnection {
        ResolvedConnection {
            name: name.map(str::to_owned),
            endpoint: endpoint.into(),
            secret: Some("lsr_do_not_display".into()),
            universe: None,
            caller: Some(api::CallerAccess {
                single: false,
                token_name: Some("operator".into()),
            }),
        }
    }

    fn universe() -> UniverseStatus {
        UniverseStatus {
            universe_id: Some(uuid::Uuid::nil().to_string()),
            handle: Some("customer-a".into()),
            unavailable_reason: None,
        }
    }

    #[test]
    fn generated_local_dev_is_quiet_but_remote_and_named_connections_stay_visible() {
        let name = format!(
            "dev-{}",
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"checkout")
        );
        for endpoint in [
            "http://localhost:18080/api/v1",
            "http://127.0.0.1/api/v1",
            "http://[::1]/api/v1",
        ] {
            let display = ConnectionDisplay::new(&connection(Some(&name), endpoint), &universe());
            assert!(display.footer.is_none());
            assert!(display.details.contains(&name));
            assert!(!display.details.contains("lsr_do_not_display"));
            assert!(
                display.header_context[1]
                    .1
                    .starts_with("local development · ")
            );
            assert!(
                display.header_context[0]
                    .1
                    .contains("customer-a · runtime token")
            );
            assert!(!display.header_context.iter().any(|(_, value)| value.contains("lsr_do_not_display") || value.contains(&name)));
        }
        let remote = ConnectionDisplay::new(
            &connection(Some(&name), "https://runtime.example/api/v1"),
            &universe(),
        );
        assert_eq!(
            remote.footer.as_deref(),
            Some("runtime.example / customer-a")
        );
        let named = ConnectionDisplay::new(
            &connection(Some("production"), "http://localhost:18080/api/v1"),
            &universe(),
        );
        assert_eq!(named.footer.as_deref(), Some("production / customer-a"));
        assert!(named.details.contains("Token: operator"));
    }

    #[test]
    fn a_selection_by_handle_shows_the_handle_when_the_uuid_cannot_be_read() {
        let mut universe = universe();
        universe.universe_id = None;
        universe.unavailable_reason = Some("runtime unavailable".into());
        let display = ConnectionDisplay::new(
            &connection(Some("production"), "https://runtime.example:8443/api/v1"),
            &universe,
        );
        assert_eq!(display.footer.as_deref(), Some("production / customer-a"));
        assert!(display.details.contains("Handle: customer-a"));
        assert!(display.details.contains("UUID: unavailable"));
        assert!(display.details.contains("runtime unavailable"));
    }

    #[test]
    fn unavailable_handles_keep_identity_and_explain_the_fallback() {
        let mut universe = universe();
        universe.handle = None;
        universe.unavailable_reason = Some("runtime unavailable".into());
        let connection = connection(None, "https://runtime.example:8443/api/v1");
        let display = ConnectionDisplay::new(&connection, &universe);
        assert_eq!(
            display.footer.as_deref(),
            Some("runtime.example:8443 / 00000000")
        );
        assert!(display.details.contains("Handle: unavailable"));
        assert!(display.details.contains("runtime unavailable"));
        assert!(display.details.contains("Authority: full runtime access"));
    }
}
