//! Catalog text rendered once by the publisher.

use crate::subagents::{AGENT_RUN_TOOL_NAME, AGENT_SPAWN_TOOL_NAME, SubagentCatalogSnapshot};

pub(crate) fn subagent_catalog_text(catalog: &SubagentCatalogSnapshot) -> String {
    let mut text = String::new();
    if catalog.agents.is_empty() {
        text.push_str("No sub-agents are currently available.");
        return text;
    }
    text.push_str(&format!(
        "You may delegate work to these agents with {AGENT_RUN_TOOL_NAME} (waits and returns the result inline; several calls in one turn run concurrently and return together) or {AGENT_SPAWN_TOOL_NAME} (returns a promise to await later). Pass the profile id as `agent`. A sub-agent sees only your brief plus its own instructions, so make each brief complete and self-contained.\n\n",
    ));
    let mut agents: Vec<_> = catalog.agents.iter().collect();
    agents.sort_by_key(|agent| &agent.profile_id);
    for agent in agents {
        let name = agent
            .display_name
            .as_deref()
            .filter(|name| !name.trim().is_empty() && *name != agent.profile_id)
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        text.push_str(&format!("- {}{name}\n", agent.profile_id));
        match agent
            .description
            .as_deref()
            .filter(|d| !d.trim().is_empty())
        {
            Some(description) => text.push_str(&format!("  {description}\n")),
            None => text.push_str("  (no description)\n"),
        }
        if agent.revision.is_none() {
            text.push_str(
                "  (this profile is currently missing; runs will fail until it is restored)\n",
            );
        }
    }
    text.push_str(&format!(
        "\nLimits for this session\'s tree: depth {}, {} descendants in total, {} running at once, {} s per child.",
        catalog.limits.max_depth,
        catalog.limits.max_descendants,
        catalog.limits.max_concurrent,
        catalog.limits.deadline_ms / 1_000,
    ));
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagents::SubagentCatalogAgent;

    #[test]
    fn catalog_text_lists_agents_descriptions_and_limits() {
        let catalog = SubagentCatalogSnapshot::new(
            vec![
                SubagentCatalogAgent {
                    profile_id: "reviewer".to_owned(),
                    display_name: Some("Reviewer".to_owned()),
                    description: Some("Reviews a PR for correctness.".to_owned()),
                    revision: Some(3),
                },
                SubagentCatalogAgent {
                    profile_id: "gone".to_owned(),
                    display_name: None,
                    description: None,
                    revision: None,
                },
            ],
            harness::SubagentLimits::default(),
        );
        let text = subagent_catalog_text(&catalog);
        assert!(text.contains("- reviewer (Reviewer)"));
        assert!(text.contains("Reviews a PR for correctness."));
        assert!(text.contains("currently missing"));
        assert!(text.contains("depth 2, 16 descendants"));
        assert!(text.contains(AGENT_RUN_TOOL_NAME));
        let mut changed = catalog.clone();
        changed.agents[0].revision = Some(99);
        changed.agents.reverse();
        assert_eq!(subagent_catalog_text(&changed), text);
    }
}
