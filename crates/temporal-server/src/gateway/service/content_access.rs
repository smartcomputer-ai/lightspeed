//! Content authorization. A content hash is never access: a blob is read
//! through a named resource the caller may read, and only when it is
//! admitted content of that resource; without a resource, only what the
//! caller uploaded. A reference the caller supplies for admission is held to
//! the same rule at that moment, so admission is where authorization happens
//! and a read is one indexed lookup. Edges are never followed.
use super::*;
use access::{Caller, Decision, ResourceRef};

fn digest_of(blob_ref: &BlobRef) -> &str {
    &blob_ref.as_str()[7..]
}

/// The references a caller placed in reference-bearing fields of a wire
/// document (camelCase) or a stored one (snake_case).
fn supplied_refs(value: &serde_json::Value) -> std::collections::BTreeSet<BlobRef> {
    fn snake(key: &str) -> String {
        let mut out = String::with_capacity(key.len() + 4);
        for ch in key.chars() {
            if ch.is_ascii_uppercase() {
                out.push('_');
                out.push(ch.to_ascii_lowercase());
            } else {
                out.push(ch);
            }
        }
        out
    }
    fn walk(value: &serde_json::Value, refs: &mut std::collections::BTreeSet<BlobRef>) {
        match value {
            serde_json::Value::Array(values) => values.iter().for_each(|value| walk(value, refs)),
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    if engine::storage::CONTENT_REF_FIELDS.contains(&snake(key).as_str())
                        && let serde_json::Value::String(text) = value
                        && let Ok(blob_ref) = BlobRef::parse(text.as_str())
                    {
                        refs.insert(blob_ref);
                    } else {
                        walk(value, refs);
                    }
                }
            }
            _ => {}
        }
    }
    let mut refs = std::collections::BTreeSet::new();
    walk(value, &mut refs);
    refs
}

impl GatewayAgentApi {
    /// Whether the current caller may read `blob_ref`; `Hidden` when the
    /// named resource itself is not visible to the caller.
    pub(super) async fn blob_read_decision(
        &self,
        resource: Option<&ResourceRef>,
        blob_ref: &BlobRef,
    ) -> Result<Decision, AgentApiError> {
        if engine::storage::engine_blob_refs().contains(blob_ref) {
            return Ok(Decision::Allowed);
        }
        let store = self.access_store();
        let digest = digest_of(blob_ref);
        if let Some(resource) = resource {
            let decision = self.decide_resource(UniverseAction::Read, resource).await?;
            if decision != Decision::Allowed {
                return Ok(decision);
            }
            return Ok(
                if store
                    .admitted_content(self.universe_id(), resource, digest)
                    .await
                    .map_err(|error| AgentApiError::internal(error.to_string()))?
                {
                    Decision::Allowed
                } else {
                    Decision::Forbidden
                },
            );
        }
        Ok(if self.supplied_ref_admissible(None, digest).await? {
            Decision::Allowed
        } else {
            Decision::Forbidden
        })
    }

    pub(super) async fn authorize_blob_read(
        &self,
        resource: Option<&ResourceRef>,
        blob_ref: &BlobRef,
    ) -> Result<(), AgentApiError> {
        match self.blob_read_decision(resource, blob_ref).await? {
            Decision::Allowed => Ok(()),
            Decision::Hidden => Err(AgentApiError::not_found("resource not found")),
            Decision::Forbidden => Err(AgentApiError::forbidden()),
        }
    }

    /// The role-then-resource decision for the current caller on an
    /// existing resource, without the creation fallback.
    async fn decide_resource(
        &self,
        action: UniverseAction,
        resource: &ResourceRef,
    ) -> Result<Decision, AgentApiError> {
        let store = self.access_store();
        let decision = match self.current_controller() {
            Some(controller) => {
                store
                    .decide(Caller::Controller(&controller), action, resource)
                    .await
            }
            None => {
                let context = self.caller()?;
                store
                    .decide(Caller::Request(&context.rights), action, resource)
                    .await
            }
        }
        .map_err(|error| AgentApiError::internal(error.to_string()))?;
        Ok(decision.unwrap_or(Decision::Hidden))
    }

    /// Whether a reference the caller supplies may be admitted into
    /// `session` (or used on its own): an engine blob, bytes the caller
    /// uploaded, content already in the target, or, for internal work,
    /// content anywhere under the actor's root.
    async fn supplied_ref_admissible(
        &self,
        session: Option<&SessionId>,
        digest: &str,
    ) -> Result<bool, AgentApiError> {
        let store = self.access_store();
        let internal = |error: access::AccessError| AgentApiError::internal(error.to_string());
        if let Some(session) = session
            && store
                .admitted_content(
                    self.universe_id(),
                    &ResourceRef::Session(session.as_str().to_owned()),
                    digest,
                )
                .await
                .map_err(internal)?
        {
            return Ok(true);
        }
        match self.current_controller() {
            Some(controller) => store
                .content_under_root(self.universe_id(), &controller.root, digest)
                .await
                .map_err(internal),
            None => {
                let principal = self.caller()?.acting_principal().id;
                store
                    .uploaded_by(self.universe_id(), principal, digest)
                    .await
                    .map_err(internal)
            }
        }
    }

    /// Every reference in a caller-supplied document must be admissible for
    /// `session`, or the whole admission is refused.
    pub(super) async fn authorize_supplied_refs(
        &self,
        session: Option<&SessionId>,
        refs: impl IntoIterator<Item = BlobRef>,
    ) -> Result<(), AgentApiError> {
        let engine_refs = engine::storage::engine_blob_refs();
        for blob_ref in refs {
            if engine_refs.contains(&blob_ref) {
                continue;
            }
            if !self
                .supplied_ref_admissible(session, digest_of(&blob_ref))
                .await?
            {
                return Err(AgentApiError::forbidden());
            }
        }
        Ok(())
    }

    /// Every reference in a document the caller wrote must be admissible
    /// for `session`; the runtime's own derivations are never checked here.
    /// Only reference-bearing fields count: text that happens to equal a
    /// digest is text.
    pub(super) async fn authorize_supplied_document(
        &self,
        session: Option<&SessionId>,
        document: &impl serde::Serialize,
    ) -> Result<(), AgentApiError> {
        // Internal work never supplies a caller's document: what a bot's
        // worker or a delegation admits, the runtime authored.
        if self.current_controller().is_some() {
            return Ok(());
        }
        let value = serde_json::to_value(document)
            .map_err(|error| AgentApiError::internal(format!("encode input: {error}")))?;
        self.authorize_supplied_refs(session, supplied_refs(&value))
            .await
    }

    /// Blobs the gateway stored from the caller's own bytes (inline text,
    /// base64 media) while converting `supplied` into `derived` are the
    /// caller's uploads: every ref in the derived document that the caller
    /// did not pass as a reference.
    pub(super) async fn record_derived_uploads(
        &self,
        supplied: &impl serde::Serialize,
        derived: &impl serde::Serialize,
    ) -> Result<(), AgentApiError> {
        let encode_error =
            |error: serde_json::Error| AgentApiError::internal(format!("encode input: {error}"));
        let supplied = engine::storage::collect_blob_refs(
            &serde_json::to_value(supplied).map_err(encode_error)?,
        );
        let derived = engine::storage::collect_blob_refs(
            &serde_json::to_value(derived).map_err(encode_error)?,
        );
        let created = derived.difference(&supplied).cloned().collect::<Vec<_>>();
        if created.is_empty() {
            return Ok(());
        }
        self.record_uploads(&created).await
    }

    /// Record that the current request's principal authored these bytes.
    /// Internal work uploads nothing through the API.
    pub(super) async fn record_uploads(&self, refs: &[BlobRef]) -> Result<(), AgentApiError> {
        if self.current_controller().is_some() {
            return Ok(());
        }
        let principal = self.caller()?.acting_principal().id;
        let digests = refs
            .iter()
            .map(|blob_ref| digest_of(blob_ref).to_owned())
            .collect::<Vec<_>>();
        self.access_store()
            .record_blob_uploads(self.universe_id(), principal, &digests, now_ms()? as u64)
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))
    }
}
