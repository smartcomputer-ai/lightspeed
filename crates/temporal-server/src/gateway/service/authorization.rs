//! Shared-service admission. No public method may rely on HTTP checks alone:
//! every handler decides from the request context resolved at the trusted
//! boundary, or from an explicit controller context for internal work. Both
//! go through `access::authorize`; storage supplies the resource facts.
use super::*;
pub(crate) use access::ControllerContext;
use access::{AccessStore as _, ActionActor, Caller, Decision, ResourceController, ResourceRef};

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
            .anchor(
                self.universe_id(),
                &ResourceRef::Session(params.session_id.clone()),
            )
            .await
            .map_err(access_error)?
            .and_then(|anchor| anchor.bot);
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

    /// A controller context for an admitted actor; an actor without an
    /// anchor holds no authority.
    async fn controller_context(
        &self,
        actor: ResourceRef,
        cause: String,
    ) -> Result<ControllerContext, AgentApiError> {
        let anchor = self
            .access_store()
            .anchor(self.universe_id(), &actor)
            .await
            .map_err(access_error)?
            .ok_or_else(denied)?;
        Ok(ControllerContext {
            universe_id: self.universe_id(),
            actor,
            root: anchor.audience_root,
            execution_principal: anchor.execution.map(|execution| execution.run_as),
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

    /// Authority of a delegated child over itself. The delegation service has
    /// already bound the child to its admitted parent invocation; this only
    /// confirms the session is a reserved delegation, never a personal one.
    pub(crate) async fn delegated_session_authority(
        &self,
        session: &SessionId,
    ) -> Result<ControllerContext, AgentApiError> {
        let resource = ResourceRef::Session(session.as_str().to_owned());
        let anchor = self
            .access_store()
            .anchor(self.universe_id(), &resource)
            .await
            .map_err(access_error)?
            .ok_or_else(denied)?;
        let (ResourceController::Session(_), ActionActor::Internal { cause, .. }) =
            (anchor.controller, anchor.created_by)
        else {
            return Err(denied());
        };
        Ok(ControllerContext {
            universe_id: self.universe_id(),
            actor: resource,
            root: anchor.audience_root,
            execution_principal: anchor.execution.map(|execution| execution.run_as),
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
            .reserve_resource(
                self.universe_id(),
                &ResourceRef::Session(child.as_str().to_owned()),
                &ActionActor::Internal {
                    component: "subagent".into(),
                    cause: cause.to_owned(),
                },
                &ResourceController::Session(parent.as_str().to_owned()),
                None,
                None,
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

    /// The execution identity a new root runs as. `service` is the
    /// universe's execution service; `personal` is the requesting person,
    /// allowed only where the universe enables it. Internal work never
    /// chooses: its resources copy their controller's.
    pub(super) async fn resolve_execution(
        &self,
        requested: Option<ExecutionInput>,
    ) -> Result<access::Execution, AgentApiError> {
        let kind = requested
            .map(|input| input.kind)
            .unwrap_or(access::ExecutionKind::Service);
        let policy = self
            .access_store()
            .universe_execution_policy(self.universe_id(), now_ms()? as u64)
            .await
            .map_err(access_error)?;
        match kind {
            access::ExecutionKind::Service => Ok(access::Execution {
                run_as: policy.execution_principal_id,
                kind,
            }),
            access::ExecutionKind::Personal => {
                let principal = self.caller()?.acting_principal().clone();
                if !policy.personal_execution_enabled
                    || principal.kind != access::PrincipalKind::User
                {
                    return Err(denied());
                }
                Ok(access::Execution {
                    run_as: principal.id,
                    kind,
                })
            }
        }
    }

    /// Admit a run: the session's execution principal must be active with
    /// resource use in the universe. The requester's own control check is
    /// separate and already done. The admission itself is the record: an
    /// API start writes an audit row, and the session's execution identity
    /// is immutable, so nothing further is stored on the run.
    pub(super) async fn admit_run(&self, session: &SessionId) -> Result<(), AgentApiError> {
        let store = self.access_store();
        let anchor = store
            .anchor(
                self.universe_id(),
                &ResourceRef::Session(session.as_str().to_owned()),
            )
            .await
            .map_err(access_error)?
            .ok_or_else(denied)?;
        let Some(execution) = anchor.execution else {
            return Err(denied());
        };
        let rights = store
            .effective_access(execution.run_as, self.scope())
            .await
            .map_err(access_error)?;
        if rights.universe_action(UniverseAction::UseResource) != RoleDecision::Allowed {
            return Err(denied());
        }
        Ok(())
    }

    /// The access summary a view carries; a resource without one was never
    /// admitted, which is an internal inconsistency, not a caller error.
    pub(super) async fn access_summary(
        &self,
        resource: &ResourceRef,
    ) -> Result<access::ResourceAccessSummary, AgentApiError> {
        self.access_store()
            .access_summary(self.universe_id(), resource)
            .await
            .map_err(access_error)?
            .ok_or_else(|| AgentApiError::internal(format!("{resource:?} has no access policy")))
    }

    /// The internal controller this request runs as, if any.
    pub(super) fn current_controller(&self) -> Option<ControllerContext> {
        CONTROLLER.try_with(Clone::clone).ok()
    }

    /// Who a list is for: the request's principal, or internal work's root.
    pub(super) fn reader(&self) -> Result<store_pg::Reader, AgentApiError> {
        if let Ok(controller) = CONTROLLER.try_with(Clone::clone) {
            return Ok(store_pg::Reader::Root(controller.root));
        }
        Ok(store_pg::Reader::Principal(
            self.caller()?.acting_principal().id,
        ))
    }

    pub(crate) async fn authorize_method(
        &self,
        method: &str,
        resource: Option<ResourceRef>,
    ) -> Result<(), AgentApiError> {
        let requirement = api::method_access(method).ok_or_else(denied)?;
        let decision = self.decide(requirement, resource.as_ref()).await?;
        if decision == Decision::Allowed {
            return Ok(());
        }
        // A resource the caller may not see is missing, not forbidden, provided
        // the caller may read universe content at all. Existing content without
        // an anchor stays refused.
        if decision == Decision::Hidden
            && let Some(resource) = &resource
            && CONTROLLER.try_with(|_| ()).is_err()
            && self.caller()?.rights.universe_action(UniverseAction::Read) == RoleDecision::Allowed
        {
            let store = self.access_store();
            let anchored = store
                .anchor(self.universe_id(), resource)
                .await
                .map_err(access_error)?
                .is_some();
            let exists = store
                .resource_exists(self.universe_id(), resource)
                .await
                .map_err(access_error)?;
            if anchored || !exists {
                let (kind, id) = match resource {
                    ResourceRef::Session(id) => ("session", id),
                    ResourceRef::Bot(id) => ("bot", id),
                    ResourceRef::Profile(id) => ("profile", id),
                    ResourceRef::Collection(id) => ("collection", id),
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
        Ok(self.decide(requirement, target).await? == Decision::Allowed)
    }

    /// One decision for the current caller. A target that has no anchor yet
    /// is decided by role alone, which is how creation of a fresh id and a
    /// request for a missing resource both resolve; an existing target is
    /// decided from its anchor, its root's policy and the caller's grant.
    async fn decide(
        &self,
        requirement: MethodAccess,
        target: Option<&ResourceRef>,
    ) -> Result<Decision, AgentApiError> {
        let controller = CONTROLLER.try_with(Clone::clone).ok();
        let context = match &controller {
            Some(controller) => {
                if controller.universe_id != self.universe_id() {
                    return Ok(Decision::Forbidden);
                }
                None
            }
            None => Some(self.caller()?),
        };
        let caller = match (&controller, &context) {
            (Some(controller), _) => Caller::Controller(controller),
            (None, Some(context)) => Caller::Request(&context.rights),
            (None, None) => unreachable!("a request without a context was refused above"),
        };
        let action = match requirement {
            MethodAccess::Universe(action) => action,
            MethodAccess::Service(capability) => {
                return Ok(match &context {
                    Some(context) if context.rights.has_capability(capability) => Decision::Allowed,
                    _ => Decision::Forbidden,
                });
            }
            _ => return Ok(Decision::Forbidden),
        };
        let Some(target) = target else {
            return Ok(access::authorize(caller, action, None));
        };
        match self
            .access_store()
            .decide(caller, action, target)
            .await
            .map_err(access_error)?
        {
            Some(decision) => Ok(decision),
            None => Ok(match access::authorize(caller, action, None) {
                Decision::Allowed => Decision::Allowed,
                _ => Decision::Hidden,
            }),
        }
    }

    /// Record who controls a new resource before it exists. Callers own what
    /// they create, or create it as a member of a collection they may write
    /// to; a bot's sessions belong to the bot.
    pub(crate) async fn reserve_resource(
        &self,
        resource: ResourceRef,
        root: Option<&ResourceRef>,
        execution: Option<access::Execution>,
    ) -> Result<(), AgentApiError> {
        if let Some(root) = root {
            if !matches!(root, ResourceRef::Collection(_)) {
                return Err(AgentApiError::invalid_request(
                    "only a collection can be the root of a new resource",
                ));
            }
            if !self
                .permitted(
                    MethodAccess::Universe(UniverseAction::ControlSession),
                    Some(root),
                )
                .await?
            {
                return Err(AgentApiError::forbidden());
            }
        }
        let (created_by, controller) = if let Ok(authority) = CONTROLLER.try_with(Clone::clone) {
            let (ResourceRef::Bot(bot), ResourceRef::Session(_), true) = (
                authority.actor,
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
            .reserve_resource(
                self.universe_id(),
                &resource,
                &created_by,
                &controller,
                root,
                execution,
                now_ms()? as u64,
            )
            .await
            .map(|_| ())
            .map_err(access_error)
    }
}
