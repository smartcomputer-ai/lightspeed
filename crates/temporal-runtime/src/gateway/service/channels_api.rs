//! `channels/*` service methods: provider accounts and pairings. The
//! inbound seam (`channels/inbound/admit`) and conversation snapshots live
//! in `crate::channels::control_plane`.

use super::*;

use ::channels::{
    ChannelAccountRecord, ChannelAccountStore, ChannelError, ChannelPairingFilter,
    ChannelPairingStore, validate_account_document,
};

pub(crate) fn map_channel_error(error: ChannelError) -> AgentApiError {
    match error {
        ChannelError::AccountAlreadyExists { .. }
        | ChannelError::AccountRevisionConflict { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        ChannelError::AccountNotFound { .. } | ChannelError::PairingNotFound { .. } => {
            AgentApiError::not_found(error.to_string())
        }
        ChannelError::InvalidInput { message } => AgentApiError::invalid_request(message),
        ChannelError::Store { message } => AgentApiError::internal(message),
    }
}

fn channel_now_ms() -> i64 {
    crate::bots::now_ms()
}

impl GatewayAgentApi {
    fn channel_accounts(&self) -> &dyn ChannelAccountStore {
        self.store.as_ref()
    }

    fn channel_pairings(&self) -> &dyn ChannelPairingStore {
        self.store.as_ref()
    }

    async fn validate_channel_account_grant(
        &self,
        record: &ChannelAccountRecord,
    ) -> Result<(), AgentApiError> {
        let Some(grant_id) = &record.document.credential_grant_id else {
            return Ok(());
        };
        let grant_id = parse_auth_grant_id(grant_id.clone())?;
        let grants: &dyn AuthGrantStore = self.store.as_ref();
        let grant = grants.read_grant(&grant_id).await.map_err(map_auth_error)?;
        require_retrievable_grant(&grant)
    }

    pub(super) async fn create_channel_account_record(
        &self,
        params: ChannelAccountCreateParams,
    ) -> Result<ChannelAccountCreateResponse, AgentApiError> {
        let ChannelAccountInput {
            account_id,
            document,
        } = params.account;
        validate_account_document(&document).map_err(map_channel_error)?;
        let record = self
            .channel_accounts()
            .create_channel_account(account_id, document, channel_now_ms())
            .await
            .map_err(map_channel_error)?;
        if let Err(error) = self.validate_channel_account_grant(&record).await {
            let _ = self
                .channel_accounts()
                .delete_channel_account(&record.account_id)
                .await;
            return Err(error);
        }
        Ok(ChannelAccountCreateResponse {
            account: record.view(),
        })
    }

    pub(super) async fn put_channel_account_record(
        &self,
        params: ChannelAccountPutParams,
    ) -> Result<ChannelAccountPutResponse, AgentApiError> {
        let ChannelAccountInput {
            account_id,
            document,
        } = params.account;
        validate_account_document(&document).map_err(map_channel_error)?;
        let record = self
            .channel_accounts()
            .put_channel_account(
                account_id,
                document,
                params.expected_revision,
                channel_now_ms(),
            )
            .await
            .map_err(map_channel_error)?;
        self.validate_channel_account_grant(&record).await?;
        Ok(ChannelAccountPutResponse {
            account: record.view(),
        })
    }

    pub(super) async fn read_channel_account_record(
        &self,
        params: ChannelAccountReadParams,
    ) -> Result<ChannelAccountReadResponse, AgentApiError> {
        let record = self
            .channel_accounts()
            .read_channel_account(&params.account_id)
            .await
            .map_err(map_channel_error)?;
        Ok(ChannelAccountReadResponse {
            account: record.view(),
        })
    }

    pub(super) async fn list_channel_account_records(
        &self,
        params: ChannelAccountListParams,
    ) -> Result<ChannelAccountListResponse, AgentApiError> {
        let records = self
            .channel_accounts()
            .list_channel_accounts(params.provider)
            .await
            .map_err(map_channel_error)?;
        Ok(ChannelAccountListResponse {
            accounts: records.iter().map(ChannelAccountRecord::view).collect(),
        })
    }

    pub(super) async fn delete_channel_account_record(
        &self,
        params: ChannelAccountDeleteParams,
    ) -> Result<ChannelAccountDeleteResponse, AgentApiError> {
        let record = self
            .channel_accounts()
            .delete_channel_account(&params.account_id)
            .await
            .map_err(map_channel_error)?;
        Ok(ChannelAccountDeleteResponse {
            account: record.view(),
        })
    }

    pub(super) async fn list_channel_pairing_records(
        &self,
        params: ChannelPairingListParams,
    ) -> Result<ChannelPairingListResponse, AgentApiError> {
        let records = self
            .channel_pairings()
            .list_channel_pairings(ChannelPairingFilter {
                account_id: params.account_id,
                bot_id: params.bot_id,
                trigger_id: None,
                chat_id: None,
            })
            .await
            .map_err(map_channel_error)?;
        Ok(ChannelPairingListResponse {
            pairings: records.iter().map(|record| record.view()).collect(),
        })
    }

    pub(super) async fn delete_channel_pairing_record(
        &self,
        params: ChannelPairingDeleteParams,
    ) -> Result<ChannelPairingDeleteResponse, AgentApiError> {
        let record = self
            .channel_pairings()
            .delete_channel_pairing(&params.account_id, &params.chat_id)
            .await
            .map_err(map_channel_error)?;
        Ok(ChannelPairingDeleteResponse {
            pairing: record.view(),
        })
    }
}
