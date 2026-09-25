//! What the runtime's own work may do. A bot or a delegated session acts
//! under a controller context and is held to these rules, decided from the
//! facts of the target's own row; requests are gated by their key's groups
//! and never decided here.
use api::{ResourceRef, UniverseAction, Visibility};
use store_pg::ResourceAccess;
use uuid::Uuid;

/// Authority of the runtime's own work for an admitted bot or delegated
/// session. It is not a bypass: it reads universe-visible work and its own
/// root, creates sessions in its own root, uses universe resources, and
/// controls only itself, its bot's sessions and the children it admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControllerContext {
    pub universe_id: Uuid,
    pub actor: ResourceRef,
    /// The actor's audience root, resolved when the context is built.
    pub root: ResourceRef,
    pub cause: String,
}

/// What internal work may do, decided from the facts of the target when
/// there is one. Requests are never decided here: whoever asserts actors
/// decides for people before the request reaches core.
pub fn authorize_controller(
    context: &ControllerContext,
    action: UniverseAction,
    target: Option<&ResourceAccess>,
) -> bool {
    use UniverseAction::*;
    let Some(target) = target else {
        return matches!(action, Read | CreateSession | UseResource);
    };
    // Profiles, workspaces, environments and MCP servers belong to the
    // universe: its work sees and uses them, and configures none of them.
    if !matches!(
        target.resource,
        ResourceRef::Session(_) | ResourceRef::Bot(_)
    ) {
        return matches!(action, Read | UseResource);
    }
    // Another root's unshared work is none of internal work's business.
    if target.audience.visibility != Visibility::Universe && target.root != context.root {
        return false;
    }
    let controls = target.resource == context.actor
        || matches!((&context.actor, &target.bot), (ResourceRef::Bot(bot), Some(managed)) if bot == managed)
        || matches!((&context.actor, &target.parent), (ResourceRef::Session(actor), Some(parent)) if actor == parent);
    match action {
        Read | CreateSession | UseResource => true,
        ControlSession | StopSession | DeleteSession | ManageBot => controls,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::ResourceAccessSummary;

    fn session(
        id: &str,
        bot: Option<&str>,
        parent: Option<&str>,
        root: ResourceRef,
        visibility: Visibility,
    ) -> ResourceAccess {
        ResourceAccess {
            resource: ResourceRef::Session(id.into()),
            bot: bot.map(str::to_owned),
            parent: parent.map(str::to_owned),
            root,
            audience: ResourceAccessSummary {
                visibility,
                created_by: None,
            },
        }
    }

    fn bot(id: &str) -> ControllerContext {
        ControllerContext {
            universe_id: Uuid::from_u128(1),
            actor: ResourceRef::Bot(id.into()),
            root: ResourceRef::Bot(id.into()),
            cause: "test".into(),
        }
    }

    #[test]
    fn a_bot_controls_its_own_sessions_and_only_reads_others() {
        use UniverseAction::*;
        let own = session(
            "bot:v1:helper:main",
            Some("helper"),
            None,
            ResourceRef::Bot("helper".into()),
            Visibility::Universe,
        );
        let other = session(
            "bot:v1:triage:main",
            Some("triage"),
            None,
            ResourceRef::Bot("triage".into()),
            Visibility::Universe,
        );
        let helper = bot("helper");
        for action in [ControlSession, StopSession, DeleteSession, Read] {
            assert_eq!(authorize_controller(&helper, action, Some(&own)), true);
        }
        assert_eq!(authorize_controller(&helper, Read, Some(&other)), true);
        for action in [ControlSession, StopSession, DeleteSession, ShareSession] {
            assert_eq!(authorize_controller(&helper, action, Some(&other)), false);
        }
        // A bot manages itself, never another bot.
        let helper_bot = ResourceAccess::shared(ResourceRef::Bot("helper".into()), None);
        let triage_bot = ResourceAccess::shared(ResourceRef::Bot("triage".into()), None);
        assert_eq!(
            authorize_controller(&helper, ManageBot, Some(&helper_bot)),
            true
        );
        assert_eq!(
            authorize_controller(&helper, ManageBot, Some(&triage_bot)),
            false
        );
    }

    #[test]
    fn internal_work_never_sees_another_roots_unshared_work() {
        let draft = ResourceRef::Session("alice-draft".into());
        let unshared = session(
            "alice-draft",
            None,
            None,
            draft.clone(),
            Visibility::Restricted,
        );
        assert_eq!(
            authorize_controller(&bot("helper"), UniverseAction::Read, Some(&unshared)),
            false
        );
        // A child of that session reads its own root.
        let child = ControllerContext {
            universe_id: Uuid::from_u128(1),
            actor: ResourceRef::Session("agent_child".into()),
            root: draft.clone(),
            cause: "delegation".into(),
        };
        assert_eq!(
            authorize_controller(&child, UniverseAction::Read, Some(&unshared)),
            true
        );
        // It controls itself, and neither its parent nor a sibling.
        let sibling = session(
            "agent_sibling",
            None,
            Some("alice-draft"),
            draft.clone(),
            Visibility::Restricted,
        );
        assert_eq!(
            authorize_controller(&child, UniverseAction::ControlSession, Some(&unshared)),
            false
        );
        assert_eq!(
            authorize_controller(&child, UniverseAction::ControlSession, Some(&sibling)),
            false
        );
        let parent = ControllerContext {
            actor: draft,
            ..child
        };
        assert_eq!(
            authorize_controller(&parent, UniverseAction::StopSession, Some(&sibling)),
            true
        );
    }

    #[test]
    fn internal_work_reads_and_uses_universe_resources_and_configures_none() {
        use UniverseAction::*;
        let environment = ResourceAccess::shared(ResourceRef::Environment("prod".into()), None);
        let helper = bot("helper");
        for (action, expected) in [
            (Read, true),
            (UseResource, true),
            (ConfigureResource, false),
        ] {
            assert_eq!(
                authorize_controller(&helper, action, Some(&environment)),
                expected
            );
        }
        for (action, expected) in [
            (Read, true),
            (CreateSession, true),
            (UseResource, true),
            (CreateBot, false),
            (ConfigureResource, false),
        ] {
            assert_eq!(authorize_controller(&helper, action, None), expected);
        }
    }
}
