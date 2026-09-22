//! Shared-service admission. No public method may rely on HTTP checks alone:
//! every handler decides from the request context resolved at the trusted
//! boundary, or from an explicit controller authority for internal work.
//! Role decisions read that context; only ownership reads storage.
use super::*;
use access::{ActionActor, ResourceController, ResourceRef};

/// Authority of internal work running for an admitted bot or delegated
/// session. It holds no roles and is not a bypass: it may read and create
/// within its universe and control only itself and the sessions of its bot.
#[derive(Clone)]
pub(crate) struct ControllerAuthority {
    pub universe_id: uuid::Uuid,
    pub resource: ResourceRef,
    pub cause: String,
}

tokio::task_local! {
    static CONTROLLER: ControllerAuthority;
}

pub(crate) async fn with_controller_authority<F: Future>(
    authority: ControllerAuthority,
    future: F,
) -> F::Output {
    CONTROLLER.scope(authority, future).await
}

fn denied() -> AgentApiError {
    AgentApiError::forbidden()
}
fn access_error(error: access::AccessError) -> AgentApiError {
    match error {
        access::AccessError::Store(message) => AgentApiError::internal(message),
        _ => denied(),
    }
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
            .ownership(
                self.universe_id(),
                &ResourceRef::Session(params.session_id.clone()),
            )
            .await
            .map_err(access_error)?
            .and_then(|ownership| ownership.bot);
        let (true, Some(bot)) = (bound, bot) else {
            return Err(denied());
        };
        with_controller_authority(
            ControllerAuthority {
                universe_id: self.universe_id(),
                resource: ResourceRef::Bot(bot),
                cause: workflow_id.to_owned(),
            },
            self.read_run(params),
        )
        .await
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
        let ResourceRef::Bot(ref id) = authority.resource else {
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

    /// Only the worker adapter constructs this authority. Bind it to the
    /// Temporal controller identity, not a session's display metadata.
    pub(crate) async fn bot_activity_authority(
        &self,
        workflow_id: &str,
        requested_bot: Option<&api::BotId>,
    ) -> Result<ControllerAuthority, AgentApiError> {
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
        let resource = ResourceRef::Bot(id.as_str().to_owned());
        // A bot without admitted ownership holds no authority.
        if self
            .access_store()
            .ownership(self.universe_id(), &resource)
            .await
            .map_err(access_error)?
            .is_none()
        {
            return Err(denied());
        }
        Ok(ControllerAuthority {
            universe_id: self.universe_id(),
            resource,
            cause: workflow_id.to_owned(),
        })
    }

    /// Authority of a delegated child over itself. The delegation service has
    /// already bound the child to its admitted parent invocation; this only
    /// confirms the session is a reserved delegation, never a personal one.
    pub(crate) async fn delegated_session_authority(
        &self,
        session: &SessionId,
    ) -> Result<ControllerAuthority, AgentApiError> {
        let resource = ResourceRef::Session(session.as_str().to_owned());
        let ownership = self
            .access_store()
            .ownership(self.universe_id(), &resource)
            .await
            .map_err(access_error)?
            .ok_or_else(denied)?;
        let (ResourceController::Session(_), ActionActor::Internal { cause, .. }) =
            (ownership.controller, ownership.created_by)
        else {
            return Err(denied());
        };
        Ok(ControllerAuthority {
            universe_id: self.universe_id(),
            resource,
            cause,
        })
    }

    /// Called only after the delegation service validates its admitted tool
    /// invocation. Reserve before the child row so origin alone grants none;
    /// the child inherits its parent's owner and bot.
    pub(crate) async fn reserve_delegated_session(
        &self,
        parent: &SessionId,
        child: &SessionId,
        cause: &str,
    ) -> Result<(), AgentApiError> {
        self.access_store()
            .reserve_ownership(
                self.universe_id(),
                &ResourceRef::Session(child.as_str().to_owned()),
                &ActionActor::Internal {
                    component: "subagent".into(),
                    cause: cause.to_owned(),
                },
                &ResourceController::Session(parent.as_str().to_owned()),
                now_ms()? as u64,
            )
            .await
            .map(|_| ())
            .map_err(access_error)
    }

    pub(crate) fn access_store(&self) -> store_pg::PgAccessStore {
        store_pg::PgAccessStore::new(self.store.pool().clone())
    }

    fn scope(&self) -> access::AccessScope {
        access::AccessScope::Universe {
            universe_id: self.universe_id(),
        }
    }

    /// The caller's context, which must address this universe-bound service.
    pub(super) fn caller(&self) -> Result<access::RequestContext, AgentApiError> {
        let context = crate::gateway::principal::request_context()?;
        if context.target_scope() != self.scope() || !context.credential_scope.permits(self.scope())
        {
            return Err(denied());
        }
        Ok(context)
    }

    /// Revalidation for a parked caller; internal controllers hold no
    /// revocable credential.
    pub(super) fn revalidation(
        &self,
    ) -> Result<Option<crate::gateway::authentication::Revalidation>, AgentApiError> {
        if CONTROLLER.try_with(|_| ()).is_ok() {
            return Ok(None);
        }
        self.caller()
            .map(crate::gateway::authentication::Revalidation::new)
            .map(Some)
    }

    pub(crate) async fn authorize_method(
        &self,
        method: &str,
        resource: Option<ResourceRef>,
    ) -> Result<(), AgentApiError> {
        let requirement = api::method_access(method).ok_or_else(denied)?;
        if self.permitted(requirement, resource.as_ref()).await? {
            return Ok(());
        }
        // An unknown target is missing, not forbidden: content is universe-visible,
        // so a reader learns nothing new. A reserved or existing target stays refused.
        if let Some(resource) = &resource
            && self
                .permitted(MethodAccess::Universe(UniverseAction::Read), None)
                .await?
        {
            let store = self.access_store();
            let known = store
                .ownership(self.universe_id(), resource)
                .await
                .map_err(access_error)?
                .is_some()
                || store
                    .resource_exists(self.universe_id(), resource)
                    .await
                    .map_err(access_error)?;
            if !known {
                let (kind, id) = match resource {
                    ResourceRef::Session(id) => ("session", id),
                    ResourceRef::Bot(id) => ("bot", id),
                    ResourceRef::Profile(id) => ("profile", id),
                };
                return Err(AgentApiError::not_found(format!("{kind} not found: {id}")));
            }
        }
        if let Ok(controller) = CONTROLLER.try_with(Clone::clone) {
            // Callers are audited at the API boundary; internal work is not a caller.
            tracing::warn!(
                target: "temporal_server",
                %method,
                cause = %controller.cause,
                "internal controller was refused"
            );
        }
        Err(denied())
    }

    /// Whether the current caller, request or internal controller, meets a
    /// requirement; handlers use it for rules beyond their method's own.
    pub(super) async fn permitted(
        &self,
        requirement: MethodAccess,
        target: Option<&ResourceRef>,
    ) -> Result<bool, AgentApiError> {
        if let Ok(controller) = CONTROLLER.try_with(Clone::clone) {
            return self
                .controller_permitted(&controller, requirement, target)
                .await;
        }
        let context = self.caller()?;
        match requirement {
            MethodAccess::Universe(action) => self
                .access_store()
                .resource_permitted(&context.rights, action, target)
                .await
                .map_err(access_error),
            MethodAccess::Service(capability) => Ok(context.rights.has_capability(capability)),
            _ => Ok(false),
        }
    }

    async fn controller_permitted(
        &self,
        authority: &ControllerAuthority,
        requirement: MethodAccess,
        target: Option<&ResourceRef>,
    ) -> Result<bool, AgentApiError> {
        if authority.universe_id != self.universe_id() {
            return Ok(false);
        }
        match requirement {
            // Catalog/content reads are universe-wide in this slice.
            MethodAccess::Universe(UniverseAction::Read | UniverseAction::CreateSession) => {
                return Ok(true);
            }
            MethodAccess::Universe(UniverseAction::UseResource)
                if matches!(authority.resource, ResourceRef::Bot(_)) =>
            {
                return Ok(true);
            }
            MethodAccess::Universe(
                UniverseAction::ControlSession
                | UniverseAction::StopSession
                | UniverseAction::DeleteSession
                | UniverseAction::ManageBot,
            ) => {}
            _ => return Ok(false),
        }
        let Some(target) = target else {
            return Ok(false);
        };
        if target == &authority.resource {
            return Ok(true);
        }
        // Beyond itself, only a bot reaches further: the sessions of its lineage.
        let ResourceRef::Bot(bot) = &authority.resource else {
            return Ok(false);
        };
        Ok(self
            .access_store()
            .ownership(self.universe_id(), target)
            .await
            .map_err(access_error)?
            .is_some_and(|ownership| ownership.bot.as_ref() == Some(bot)))
    }

    /// Record who controls a new resource before it exists. Callers own what
    /// they create; a bot's sessions belong to the bot.
    pub(crate) async fn reserve_resource(
        &self,
        resource: ResourceRef,
    ) -> Result<(), AgentApiError> {
        let (created_by, controller) = if let Ok(authority) = CONTROLLER.try_with(Clone::clone) {
            let (ResourceRef::Bot(bot), ResourceRef::Session(_), true) = (
                authority.resource,
                &resource,
                authority.universe_id == self.universe_id(),
            ) else {
                return Err(denied());
            };
            (
                ActionActor::Internal {
                    component: "controller".into(),
                    cause: authority.cause,
                },
                ResourceController::Bot(bot),
            )
        } else {
            let principal = self.caller()?.acting_principal().id;
            if matches!(&resource, ResourceRef::Session(id) if id.starts_with("bot:v1:") || id.starts_with("agent_"))
            {
                return Err(denied());
            }
            (
                ActionActor::Principal { id: principal },
                ResourceController::Principal(principal),
            )
        };
        self.access_store()
            .reserve_ownership(
                self.universe_id(),
                &resource,
                &created_by,
                &controller,
                now_ms()? as u64,
            )
            .await
            .map(|_| ())
            .map_err(access_error)
    }
}
