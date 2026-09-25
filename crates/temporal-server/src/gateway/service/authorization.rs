//! Shared-service admission. Every handler authorizes by its method name:
//! a request must address this universe with a key holding the method's
//! group, which the boundary already checked and in-process callers must
//! satisfy too; internal work for a bot or a delegated session is held to
//! the controller rules, from the target's own row. Deciding what
//! a person may do belongs to whoever asserts actors.
pub(crate) use super::controller::ControllerContext;
use super::controller::authorize_controller;
use super::*;
use crate::gateway::request_context::RequestContext;
use store_pg::ResourceAccess;

tokio::task_local! {
    static CONTROLLER: ControllerContext;
}

pub(crate) async fn with_controller_authority<F: Future>(
    authority: ControllerContext,
    future: F,
) -> F::Output {
    CONTROLLER.scope(authority, future).await
}

fn denied() -> AgentApiError {
    AgentApiError::forbidden()
}
fn access_error(error: store_pg::AccessStoreError) -> AgentApiError {
    AgentApiError::internal(error.to_string())
}

impl GatewayAgentApi {
    /// A channel conversation reads the runs of sessions that admitted it as a
    /// bound receiver; it acts with the authority of the bot that owns them.
    pub(crate) async fn read_run_for_channel(
        &self,
        workflow_id: &str,
        params: RunReadParams,
    ) -> Result<AgentApiOutcome<RunReadResponse>, AgentApiError> {
        if !workflow_id.starts_with(&format!("{}/chat-", self.universe_id())) {
            return Err(denied());
        }
        let session = SessionId::try_new(params.session_id.clone()).map_err(|_| denied())?;
        let loaded = self.load_session_state(&session).await?;
        let bound = loaded.state.workflow_tools.bindings.values().any(|binding| matches!(&binding.target,
            engine::WorkflowToolTarget::Bound { receiver, .. }
                if receiver.workflow_id == workflow_id && receiver.workflow_kind == temporal_workflow::channels::CHANNEL_CONVERSATION_WORKFLOW_KIND));
        let bot = self
            .access_store()
            .session_access(self.universe_id(), &params.session_id)
            .await
            .map_err(access_error)?
            .and_then(|access| access.bot);
        let (true, Some(bot)) = (bound, bot) else {
            return Err(denied());
        };
        let context = self
            .controller_context(ResourceRef::Bot(bot), workflow_id.to_owned())
            .await?;
        with_controller_authority(context, self.read_run(params)).await
    }

    /// Cascade deletion checks every session of the retention tree.
    pub(super) async fn authorize_session_subtree(
        &self,
        session: &str,
        method: &str,
    ) -> Result<(), AgentApiError> {
        let ids = self
            .access_store()
            .session_deletion_targets(self.universe_id(), session)
            .await
            .map_err(access_error)?;
        for id in ids {
            self.authorize_method(method, Some(ResourceRef::Session(id)))
                .await?;
        }
        Ok(())
    }

    pub(super) async fn may_control_session(
        &self,
        session: &SessionId,
    ) -> Result<bool, AgentApiError> {
        self.permitted(
            MethodAccess::Universe(UniverseAction::ControlSession),
            Some(&ResourceRef::Session(session.as_str().to_owned())),
        )
        .await
    }

    pub(crate) async fn may_manage_bot(&self, id: &api::BotId) -> Result<bool, AgentApiError> {
        self.permitted(
            MethodAccess::Universe(UniverseAction::ManageBot),
            Some(&ResourceRef::Bot(id.as_str().into())),
        )
        .await
    }

    /// A bot's poll trigger leases exactly the credential its admitted
    /// configuration names, under the bot's own controller authority.
    pub(crate) async fn lease_bot_poll_grant(
        &self,
        params: AuthGrantLeaseParams,
    ) -> Result<AgentApiOutcome<AuthGrantLeaseResponse>, AgentApiError> {
        use ::bots::BotTriggerStore as _;
        let authority = CONTROLLER.try_with(Clone::clone).map_err(|_| denied())?;
        let ResourceRef::Bot(ref id) = authority.actor else {
            return Err(denied());
        };
        if authority.universe_id != self.universe_id() {
            return Err(denied());
        }
        let bot_id = api::BotId::try_new(id.clone()).map_err(|_| denied())?;
        let triggers = self
            .store
            .list_bot_triggers(&bot_id)
            .await
            .map_err(crate::bots::map_bot_error)?;
        let configured = triggers.iter().any(|trigger| {
            matches!(&trigger.document.spec,
            BotTriggerSpec::Poll { source: PollSource::Http { auth: Some(auth), .. }, .. }
            if auth.grant_id == params.grant_id && auth.audience == params.audience)
        });
        if !configured {
            return Err(denied());
        }
        self.lease_grant_token(params).await
    }

    /// A controller context for an actor that exists: a bot is its own
    /// root; a delegated session follows its root.
    async fn controller_context(
        &self,
        actor: ResourceRef,
        cause: String,
    ) -> Result<ControllerContext, AgentApiError> {
        let access = self.target_access(&actor).await?.ok_or_else(denied)?;
        Ok(ControllerContext {
            universe_id: self.universe_id(),
            actor,
            root: access.root,
            cause,
        })
    }

    /// Only the worker adapter constructs this authority. Bind it to the
    /// Temporal controller identity, not a session's display metadata.
    pub(crate) async fn bot_activity_authority(
        &self,
        workflow_id: &str,
        requested_bot: Option<&api::BotId>,
    ) -> Result<ControllerContext, AgentApiError> {
        let prefix = format!("{}/bot-", self.universe_id());
        let id = if let Some(id) = workflow_id.strip_prefix(&prefix) {
            let id = api::BotId::try_new(id.to_owned()).map_err(|_| denied())?;
            if requested_bot.is_some_and(|requested| requested != &id) {
                return Err(denied());
            }
            id
        } else {
            let id = requested_bot.ok_or_else(denied)?;
            if !workflow_id.starts_with(&format!("{}/botfire-{}-", self.universe_id(), id)) {
                return Err(denied());
            }
            id.clone()
        };
        self.controller_context(
            ResourceRef::Bot(id.as_str().to_owned()),
            workflow_id.to_owned(),
        )
        .await
    }

    /// Authority of a delegated child over itself. The delegation service
    /// has already bound the child to its admitted parent invocation; this
    /// only confirms the session was created as a delegation, which only the
    /// runtime does.
    pub(crate) async fn delegated_session_authority(
        &self,
        session: &SessionId,
    ) -> Result<ControllerContext, AgentApiError> {
        let access = self
            .access_store()
            .session_access(self.universe_id(), session.as_str())
            .await
            .map_err(access_error)?
            .filter(|access| access.parent.is_some())
            .ok_or_else(denied)?;
        Ok(ControllerContext {
            universe_id: self.universe_id(),
            cause: format!("delegated session {session}"),
            actor: access.resource,
            root: access.root,
        })
    }

    pub(crate) fn access_store(&self) -> store_pg::PgAccessStore {
        store_pg::PgAccessStore::new(self.store.pool().clone())
    }

    /// The caller's context, which must address this universe-bound service.
    pub(super) fn caller(&self) -> Result<RequestContext, AgentApiError> {
        let context = crate::gateway::request_context::request_context()?;
        if context.scope
            != (AccessScope::Universe {
                universe_id: self.universe_id(),
            })
        {
            return Err(denied());
        }
        Ok(context)
    }

    /// The access facts of a target: a session's or bot's from its row
    /// (`None` when it does not exist); any other resource belongs to the
    /// universe.
    pub(super) async fn target_access(
        &self,
        target: &ResourceRef,
    ) -> Result<Option<ResourceAccess>, AgentApiError> {
        let store = self.access_store();
        match target {
            ResourceRef::Session(id) => store.session_access(self.universe_id(), id).await,
            ResourceRef::Bot(id) => store.bot_access(self.universe_id(), id).await,
            other => Ok(Some(ResourceAccess::shared(other.clone(), None))),
        }
        .map_err(access_error)
    }

    /// The audience a session or bot view carries.
    pub(super) async fn access_summary(
        &self,
        resource: &ResourceRef,
    ) -> Result<ResourceAccessSummary, AgentApiError> {
        self.target_access(resource)
            .await?
            .map(|access| access.audience)
            .ok_or_else(|| not_found(resource))
    }

    /// The internal controller this request runs as, if any.
    pub(super) fn current_controller(&self) -> Option<ControllerContext> {
        CONTROLLER.try_with(Clone::clone).ok()
    }

    /// What a list of sessions is narrowed to for the current work: the
    /// request's own filter, and for internal work shared work and its own
    /// root only.
    pub(super) fn access_filter(
        &self,
        mut requested: store_pg::AccessFilter,
    ) -> store_pg::AccessFilter {
        if let Some(controller) = self.current_controller() {
            requested.within_root = Some(controller.root);
        }
        requested
    }

    /// Admission of what a configuration attaches: every workspace,
    /// environment and MCP server it names must exist in the universe.
    /// Removing a resource is never checked, only what remains.
    pub(super) async fn admit_attachments(
        &self,
        features: &engine::FeaturesConfig,
    ) -> Result<(), AgentApiError> {
        require_resources(
            &self.access_store(),
            self.universe_id(),
            &temporal_workflow::attached_resources(features),
        )
        .await
    }

    pub(crate) async fn authorize_method(
        &self,
        method: &str,
        resource: Option<ResourceRef>,
    ) -> Result<(), AgentApiError> {
        let requirement = api::method_access(method).ok_or_else(denied)?;
        let Some(controller) = self.current_controller() else {
            if !self.caller()?.permits(method) {
                return Err(denied());
            }
            return Ok(());
        };
        if self
            .permits_controller(&controller, requirement, resource.as_ref())
            .await?
        {
            return Ok(());
        }
        // A refusal of internal work is a runtime fault worth a log line.
        tracing::warn!(
            target: "temporal_server",
            %method,
            cause = %controller.cause,
            "internal controller was refused"
        );
        Err(denied())
    }

    /// Whether the current caller, request or internal controller, meets a
    /// requirement; handlers use it for rules beyond their method's own.
    pub(super) async fn permitted(
        &self,
        requirement: MethodAccess,
        target: Option<&ResourceRef>,
    ) -> Result<bool, AgentApiError> {
        match self.current_controller() {
            Some(controller) => Ok(self
                .permits_controller(&controller, requirement, target)
                .await?),
            None => self.caller().map(|_| true),
        }
    }

    /// Internal work's permission on a requirement. A session that does not
    /// exist yet is decided as no target, which is how a bot starting its
    /// session resolves.
    async fn permits_controller(
        &self,
        controller: &ControllerContext,
        requirement: MethodAccess,
        target: Option<&ResourceRef>,
    ) -> Result<bool, AgentApiError> {
        let MethodAccess::Universe(action) = requirement else {
            return Ok(false);
        };
        if controller.universe_id != self.universe_id() {
            return Ok(false);
        }
        let access = match target {
            Some(target) => self.target_access(target).await?,
            None => None,
        };
        Ok(authorize_controller(controller, action, access.as_ref()))
    }

    /// Who a change is attributed to: internal work names its controller; a
    /// request, its actor, else its key, else the local caller.
    pub(super) fn attribution(&self) -> Result<Attribution, AgentApiError> {
        Ok(match self.current_controller() {
            Some(controller) => Attribution::Internal {
                component: controller.actor.kind().into(),
                cause: controller.cause,
            },
            None => self.caller()?.attribution(),
        })
    }

    /// The attribution as engine commands record it.
    pub(super) fn requested_by(&self) -> Result<Option<engine::Attribution>, AgentApiError> {
        Ok(Some(match self.attribution()? {
            Attribution::Actor { id } => engine::Attribution::Actor { id },
            Attribution::Key { prefix } => engine::Attribution::Key { prefix },
            Attribution::Local => engine::Attribution::Local,
            Attribution::Internal { component, cause } => {
                engine::Attribution::Internal { component, cause }
            }
        }))
    }

    /// Record who started a session, once its row exists. A request's root
    /// is unshared unless it asked otherwise; a bot's session belongs to the
    /// bot; a delegated child follows its root and is not stamped.
    pub(super) async fn record_session_creator(
        &self,
        session: &SessionId,
        visibility: Option<Visibility>,
    ) -> Result<(), AgentApiError> {
        let (created_by, visibility, bot) = match self.current_controller() {
            Some(controller) => match controller.actor {
                ResourceRef::Bot(bot) => (
                    Attribution::Internal {
                        component: "controller".into(),
                        cause: controller.cause,
                    },
                    None,
                    Some(bot),
                ),
                _ => return Ok(()),
            },
            None => (
                self.caller()?.attribution(),
                Some(visibility.unwrap_or(Visibility::Restricted)),
                None,
            ),
        };
        self.access_store()
            .stamp_session(
                self.universe_id(),
                session.as_str(),
                &created_by,
                visibility,
                bot.as_deref(),
            )
            .await
            .map_err(access_error)
    }

    /// Record who created a bot, once its row exists.
    pub(super) async fn record_bot_creator(&self, bot: &api::BotId) -> Result<(), AgentApiError> {
        let created_by = self.caller()?.attribution();
        self.access_store()
            .stamp_bot(self.universe_id(), bot.as_str(), &created_by)
            .await
            .map_err(access_error)
    }
}

/// Every one of `resources` exists in `universe`, or the first missing one
/// is not found.
pub(crate) async fn require_resources(
    store: &store_pg::PgAccessStore,
    universe: uuid::Uuid,
    resources: &[ResourceRef],
) -> Result<(), AgentApiError> {
    for resource in resources {
        if !store
            .resource_exists(universe, resource)
            .await
            .map_err(access_error)?
        {
            return Err(not_found(resource));
        }
    }
    Ok(())
}

/// A resource that does not exist: one text for every kind, naming only the
/// kind and the id that was supplied.
pub(crate) fn not_found(resource: &ResourceRef) -> AgentApiError {
    AgentApiError::not_found(format!("{} not found: {}", resource.label(), resource.id()))
}
