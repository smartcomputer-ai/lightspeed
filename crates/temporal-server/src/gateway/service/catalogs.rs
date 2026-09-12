use super::*;

/// Public context edits cannot write or remove runtime-owned slots.
pub(super) fn parse_client_context_key(value: String) -> Result<ContextEntryKey, AgentApiError> {
    let key = ContextEntryKey::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid context key: {error}")))?;
    if key.as_str() == "runtime" || key.as_str().starts_with("runtime.") {
        return Err(AgentApiError::invalid_request(
            "runtime context keys are owned by the runtime",
        ));
    }
    engine::validate_external_context_key(&key)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid context key: {error}")))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tools::catalog::{SUBAGENT_CATALOG_CONTEXT_KEY, VFS_CATALOG_CONTEXT_KEY};

    #[test]
    fn client_context_keys_reserve_runtime_namespaces() {
        for key in [
            "runtime",
            "runtime.catalog",
            VFS_CATALOG_CONTEXT_KEY,
            SKILL_CATALOG_CONTEXT_KEY,
            SUBAGENT_CATALOG_CONTEXT_KEY,
            "runtime.catalog.skills.environment",
            "run",
            "run.1",
        ] {
            let error = parse_client_context_key(key.to_owned()).expect_err(key);
            assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
        }
        for key in [
            "bot:directory",
            "client.catalog",
            "runtime-info",
            "runtime_extra",
            "instructions.client",
        ] {
            assert_eq!(
                parse_client_context_key(key.to_owned()).unwrap().as_str(),
                key
            );
        }
    }
}
