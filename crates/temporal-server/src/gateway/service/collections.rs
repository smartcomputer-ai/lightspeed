//! Collections: a named root that gives the sessions and bots created in it
//! one audience. The anchor and policy are reserved like any root; the
//! collection row holds the name. Nothing creates one implicitly.
use super::*;

fn collection_view(record: store_pg::CollectionRecord) -> CollectionView {
    CollectionView {
        collection_id: record.collection_id,
        display_name: record.display_name,
        revision: record.revision,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

fn map_collection_error(error: access::AccessError) -> AgentApiError {
    match error {
        access::AccessError::Invalid(message) => AgentApiError::invalid_request(message),
        access::AccessError::NotFound => AgentApiError::not_found("collection not found"),
        access::AccessError::Conflict => {
            AgentApiError::conflict("collection still holds sessions or bots; delete them first")
        }
        access::AccessError::RevisionMismatch { expected, actual } => AgentApiError::conflict(
            format!("collection revision is {actual}, expected {expected}"),
        ),
        access::AccessError::Denied => AgentApiError::forbidden(),
        other => AgentApiError::internal(other.to_string()),
    }
}

fn validate_display_name(display_name: &str) -> Result<(), AgentApiError> {
    if display_name.trim().is_empty() || display_name.len() > 200 {
        return Err(AgentApiError::invalid_request(
            "displayName must be 1 to 200 bytes",
        ));
    }
    Ok(())
}

impl GatewayAgentApi {
    /// Reserve, apply the audience, and write the row. Returns the root
    /// reference for members created afterwards.
    pub(super) async fn create_collection_record(
        &self,
        collection_id: Option<String>,
        display_name: &str,
        access: Option<AccessInput>,
    ) -> Result<ResourceRef, AgentApiError> {
        validate_display_name(display_name)?;
        if access.as_ref().is_some_and(|access| access.root.is_some()) {
            return Err(AgentApiError::invalid_request(
                "a collection is a root and cannot be created inside another",
            ));
        }
        let collection_id = match collection_id {
            Some(id) => {
                api::validate_session_id(&id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid collection id: {error}"))
                })?;
                id
            }
            None => format!("collection_{}", uuid::Uuid::new_v4().simple()),
        };
        let resource = ResourceRef::Collection(collection_id.clone());
        if self
            .access_store()
            .anchor(self.universe_id(), &resource)
            .await
            .map_err(map_collection_error)?
            .is_some()
        {
            return Err(AgentApiError::conflict(format!(
                "collection {collection_id} already exists"
            )));
        }
        self.reserve_resource(resource.clone(), None).await?;
        self.apply_creation_access(&resource, access).await?;
        self.access_store()
            .create_collection(
                self.universe_id(),
                &collection_id,
                display_name,
                now_ms()? as u64,
            )
            .await
            .map_err(map_collection_error)?;
        Ok(resource)
    }

    pub(super) async fn create_collection_response(
        &self,
        params: CollectionCreateParams,
    ) -> Result<CollectionCreateResponse, AgentApiError> {
        let ResourceRef::Collection(collection_id) = self
            .create_collection_record(params.collection_id, &params.display_name, params.access)
            .await?
        else {
            unreachable!("collection creation returns a collection reference")
        };
        let record = self
            .access_store()
            .read_collection(self.universe_id(), &collection_id)
            .await
            .map_err(map_collection_error)?
            .ok_or_else(|| AgentApiError::not_found("collection not found"))?;
        Ok(CollectionCreateResponse {
            collection: collection_view(record),
        })
    }

    pub(super) async fn read_collection_response(
        &self,
        params: CollectionReadParams,
    ) -> Result<CollectionReadResponse, AgentApiError> {
        let store = self.access_store();
        let record = store
            .read_collection(self.universe_id(), &params.collection_id)
            .await
            .map_err(map_collection_error)?
            .ok_or_else(|| AgentApiError::not_found("collection not found"))?;
        let members = store
            .collection_members(self.universe_id(), &params.collection_id)
            .await
            .map_err(map_collection_error)?;
        Ok(CollectionReadResponse {
            collection: collection_view(record),
            members,
        })
    }

    pub(super) async fn list_collections_response(
        &self,
    ) -> Result<CollectionListResponse, AgentApiError> {
        let reader = self.reader()?;
        let collections = self
            .access_store()
            .list_collections(self.universe_id(), &reader)
            .await
            .map_err(map_collection_error)?
            .into_iter()
            .map(collection_view)
            .collect();
        Ok(CollectionListResponse { collections })
    }

    pub(super) async fn update_collection_response(
        &self,
        params: CollectionUpdateParams,
    ) -> Result<CollectionUpdateResponse, AgentApiError> {
        validate_display_name(&params.display_name)?;
        let record = self
            .access_store()
            .update_collection(
                self.universe_id(),
                &params.collection_id,
                &params.display_name,
                params.expected_revision,
                now_ms()? as u64,
            )
            .await
            .map_err(map_collection_error)?;
        Ok(CollectionUpdateResponse {
            collection: collection_view(record),
        })
    }

    pub(super) async fn delete_collection_response(
        &self,
        params: CollectionDeleteParams,
    ) -> Result<CollectionDeleteResponse, AgentApiError> {
        let record = self
            .access_store()
            .delete_collection(self.universe_id(), &params.collection_id)
            .await
            .map_err(map_collection_error)?;
        Ok(CollectionDeleteResponse {
            collection: collection_view(record),
        })
    }
}
