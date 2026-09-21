//! Shared-service admission. No public method may rely on HTTP checks alone.
use super::*;
use access::{AccessStore as _, ActionActor, ResourceController, ResourceOwnership, ResourceRef};

#[derive(Clone)]
pub(crate) struct ControllerAuthority {
    pub universe_id: uuid::Uuid,
    pub resource: ResourceRef,
    pub cause: String,
    pub read_only: bool,
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
    AgentApiError::rejected("request is not authorized")
}
fn access_error(error: access::AccessError) -> AgentApiError {
    match error {
        access::AccessError::Store(message) => AgentApiError::internal(message),
        _ => denied(),
    }
}

impl GatewayAgentApi {
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
        let resource = ResourceRef::Session(params.session_id.clone());
        let chain = self
            .access_store()
            .controller_chain(self.universe_id(), &resource)
            .await
            .map_err(access_error)?;
        if !bound
            || !chain
                .iter()
                .any(|c| matches!(c, ResourceController::Bot(_)))
        {
            return Err(denied());
        }
        with_controller_authority(
            ControllerAuthority {
                universe_id: self.universe_id(),
                resource,
                cause: workflow_id.to_owned(),
                read_only: true,
            },
            self.read_run(params),
        )
        .await
    }
    pub(super) async fn authorize_session_subtree(
        &self,
        session: &str,
        method: &str,
    ) -> Result<(), AgentApiError> {
        let ids: Vec<String> = sqlx::query_scalar(
            "WITH RECURSIVE tree(session_id) AS (
            SELECT $2::text UNION SELECT child.session_id FROM sessions child JOIN tree parent
            ON (child.source_seq IS NOT NULL AND child.source_session_id=parent.session_id)
            OR child.origin_parent_session_id=parent.session_id WHERE child.universe_id=$1)
            SELECT session_id FROM tree",
        )
        .bind(self.universe_id())
        .bind(session)
        .fetch_all(self.store.pool())
        .await
        .map_err(|e| AgentApiError::internal(e.to_string()))?;
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
        let target = ResourceRef::Session(session.as_str().to_owned());
        if let Ok(controller) = CONTROLLER.try_with(Clone::clone) {
            return self
                .controller_permitted(
                    &controller,
                    MethodAccess::Universe(UniverseAction::ControlSession),
                    Some(&target),
                )
                .await;
        }
        let (_, rights) = self.current_rights().await?;
        self.access_store()
            .resource_permitted(&rights, UniverseAction::ControlSession, Some(&target))
            .await
            .map_err(access_error)
    }

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
        self.access_store()
            .record_action_admission(
                self.universe_id(),
                &ActionActor::Internal {
                    component: "bot_poll".into(),
                    cause: authority.cause,
                },
                None,
                METHOD_AUTH_GRANTS_LEASE,
                Some(&authority.resource),
                None,
                true,
                now_ms()? as u64,
            )
            .await
            .map_err(access_error)?;
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
            read_only: false,
        })
    }

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
        if !matches!(ownership.controller, ResourceController::Session(_)) {
            return Err(denied());
        }
        let ActionActor::Internal { cause, .. } = ownership.created_by else {
            return Err(denied());
        };
        Ok(ControllerAuthority {
            universe_id: self.universe_id(),
            resource,
            cause,
            read_only: false,
        })
    }

    pub(crate) async fn reserve_delegated_session(
        &self,
        parent: &SessionId,
        child: &SessionId,
        cause: &str,
    ) -> Result<(), AgentApiError> {
        // Called only after the delegation service validates its admitted tool
        // invocation. Reserve before the child row so origin alone grants none.
        self.access_store()
            .controller_chain(
                self.universe_id(),
                &ResourceRef::Session(parent.as_str().to_owned()),
            )
            .await
            .map_err(access_error)?;
        self.access_store()
            .reserve_ownership(
                self.universe_id(),
                &ResourceOwnership {
                    resource: ResourceRef::Session(child.as_str().to_owned()),
                    created_by: ActionActor::Internal {
                        component: "subagent".into(),
                        cause: cause.to_owned(),
                    },
                    controller: ResourceController::Session(parent.as_str().to_owned()),
                    created_at_ms: now_ms()? as u64,
                },
            )
            .await
            .map_err(access_error)
    }

    pub(crate) fn access_store(&self) -> store_pg::PgAccessStore {
        store_pg::PgAccessStore::new(self.store.pool().clone())
    }

    async fn current_rights(
        &self,
    ) -> Result<(access::RequestContext, access::EffectiveAccess), AgentApiError> {
        let context = crate::gateway::principal::request_context()?;
        let scope = access::AccessScope::Universe {
            universe_id: self.universe_id(),
        };
        if context.target_scope != scope
            || !matches!(context.credential_scope, access::AccessScope::Deployment)
                && context.credential_scope != scope
        {
            return Err(denied());
        }
        let store = self.access_store();
        let authenticated = store
            .principal(context.authenticated_principal.id)
            .await
            .map_err(access_error)?
            .filter(|p| p.status == access::PrincipalStatus::Active)
            .ok_or_else(denied)?;
        if let access::AuthenticationReference::ApiKey { ref key_prefix } = context.authentication {
            let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_keys WHERE key_prefix=$1 AND principal_id=$2 AND universe_id IS NOT DISTINCT FROM $3 AND revoked_at_ms IS NULL)")
                .bind(key_prefix).bind(authenticated.id)
                .bind(match context.credential_scope { access::AccessScope::Deployment => None, access::AccessScope::Universe { universe_id } => Some(universe_id) })
                .fetch_one(self.store.pool()).await.map_err(|e| AgentApiError::internal(e.to_string()))?;
            if !active {
                return Err(denied());
            }
        }
        if authenticated.id != context.acting_principal.id {
            let scoped = store
                .effective_access(authenticated.id, scope)
                .await
                .map_err(access_error)?;
            let deployment = context.credential_scope == access::AccessScope::Deployment
                && store
                    .effective_access(authenticated.id, access::AccessScope::Deployment)
                    .await
                    .map_err(access_error)?
                    .has_capability(access::ServiceCapability::AssertUser);
            if !scoped.has_capability(access::ServiceCapability::AssertUser) && !deployment {
                return Err(denied());
            }
            if context.acting_principal.kind != access::PrincipalKind::User {
                return Err(denied());
            }
        }
        let rights = store
            .effective_access(context.acting_principal.id, scope)
            .await
            .map_err(access_error)?;
        Ok((context, rights))
    }

    pub(crate) async fn authorize_method(
        &self,
        method: &str,
        resource: Option<ResourceRef>,
    ) -> Result<(), AgentApiError> {
        let requirement = api::method_access(method).ok_or_else(denied)?;
        let store = self.access_store();
        let (actor, context, revision, allowed) =
            if let Ok(controller) = CONTROLLER.try_with(Clone::clone) {
                let allowed = self
                    .controller_permitted(&controller, requirement, resource.as_ref())
                    .await?;
                (
                    ActionActor::Internal {
                        component: "controller".into(),
                        cause: controller.cause,
                    },
                    None,
                    None,
                    allowed,
                )
            } else {
                let (context, rights) = self.current_rights().await?;
                let allowed = match requirement {
                    MethodAccess::Universe(action) => store
                        .resource_permitted(&rights, action, resource.as_ref())
                        .await
                        .map_err(access_error)?,
                    MethodAccess::Service(capability) => rights.has_capability(capability),
                    _ => false,
                };
                (
                    ActionActor::Principal {
                        id: rights.principal.id,
                    },
                    Some(context),
                    Some(rights.policy_revision),
                    allowed,
                )
            };
        if !allowed || !matches!(requirement, MethodAccess::Universe(UniverseAction::Read)) {
            store
                .record_action_admission(
                    self.universe_id(),
                    &actor,
                    context.as_ref(),
                    method,
                    resource.as_ref(),
                    revision,
                    allowed,
                    now_ms()? as u64,
                )
                .await
                .map_err(access_error)?;
        }
        if allowed { Ok(()) } else { Err(denied()) }
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
        if authority.read_only {
            return Ok(
                matches!(requirement, MethodAccess::Universe(UniverseAction::Read))
                    && target == Some(&authority.resource),
            );
        }
        let store = self.access_store();
        if store
            .ownership(self.universe_id(), &authority.resource)
            .await
            .map_err(access_error)?
            .is_none()
        {
            return Ok(false);
        }
        match requirement {
            // Catalog/content reads are universe-wide in this slice.
            MethodAccess::Universe(UniverseAction::Read) => return Ok(true),
            MethodAccess::Universe(UniverseAction::CreateSession) => return Ok(true),
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
        let expected = match &authority.resource {
            ResourceRef::Bot(id) => ResourceController::Bot(id.clone()),
            ResourceRef::Session(id) => ResourceController::Session(id.clone()),
            ResourceRef::Profile(_) => return Ok(false),
        };
        match store.controller_chain(self.universe_id(), target).await {
            Ok(chain) => Ok(chain.contains(&expected)),
            Err(access::AccessError::Denied) => Ok(false),
            Err(error) => Err(access_error(error)),
        }
    }

    pub(crate) async fn reserve_resource(
        &self,
        resource: ResourceRef,
    ) -> Result<(), AgentApiError> {
        let (created_by, controller) = if let Ok(authority) = CONTROLLER.try_with(Clone::clone) {
            if authority.universe_id != self.universe_id()
                || authority.read_only
                || !matches!(resource, ResourceRef::Session(_))
            {
                return Err(denied());
            }
            let controller = match authority.resource {
                ResourceRef::Bot(id) => ResourceController::Bot(id),
                ResourceRef::Session(id) => ResourceController::Session(id),
                _ => return Err(denied()),
            };
            (
                ActionActor::Internal {
                    component: "controller".into(),
                    cause: authority.cause,
                },
                controller,
            )
        } else {
            let context = crate::gateway::principal::request_context()?;
            if matches!(&resource, ResourceRef::Session(id) if id.starts_with("bot:v1:") || id.starts_with("agent_"))
            {
                return Err(denied());
            }
            (
                ActionActor::Principal {
                    id: context.acting_principal.id,
                },
                ResourceController::Principal(context.acting_principal.id),
            )
        };
        self.access_store()
            .reserve_ownership(
                self.universe_id(),
                &ResourceOwnership {
                    resource,
                    created_by,
                    controller,
                    created_at_ms: now_ms()? as u64,
                },
            )
            .await
            .map_err(access_error)
    }

    pub(crate) async fn may_manage_bot(&self, id: &api::BotId) -> Result<bool, AgentApiError> {
        let target = ResourceRef::Bot(id.as_str().into());
        if let Ok(controller) = CONTROLLER.try_with(Clone::clone) {
            return self
                .controller_permitted(
                    &controller,
                    MethodAccess::Universe(UniverseAction::ManageBot),
                    Some(&target),
                )
                .await;
        }
        let (_, rights) = self.current_rights().await?;
        self.access_store()
            .resource_permitted(&rights, UniverseAction::ManageBot, Some(&target))
            .await
            .map_err(access_error)
    }
}
