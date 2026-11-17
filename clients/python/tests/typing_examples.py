from typing import assert_type
from collections.abc import AsyncIterator
from lightspeed_ai import types as Api

from lightspeed_ai import AsyncLightspeedClient, ClientConnectionResponse
from lightspeed_ai import platform_types as Platform
from lightspeed_ai.types import (
    CredentialGrantPutResponse,
    McpServerPutResponse,
    RunReadResponse,
    SessionEventKindViewRunFailed,
    SessionEventView,
    SessionStartResponse,
)


async def use_client(client: AsyncLightspeedClient) -> None:
    assert_type(await client.start_session(display_name="SDK", delete_after_close_ms=None), SessionStartResponse)
    assert_type(await client.call("session/runs/read", {"sessionId": "s", "runId": "r"}), RunReadResponse)


async def configure_mcp_server(client: AsyncLightspeedClient) -> None:
    assert_type(
        await client.call("mcp/servers/put", {
            "server": {
                "serverId": "example",
                "serverUrl": "https://mcp.example.test/mcp",
                "defaultServerLabel": "Example",
                "headers": {
                    "X-Workspace": {"type": "literal", "value": "demo"},
                    "Authorization": {"type": "credential", "grantId": "grant"},
                },
            },
        }),
        McpServerPutResponse,
    )
    assert_type(
        await client.call("credentials/grants/put", {"grantId": "grant", "token": "fixture-token"}),
        CredentialGrantPutResponse,
    )


def failure_message(event: SessionEventView) -> str | None:
    kind = event["kind"]
    if kind["type"] == "runFailed":
        assert_type(kind, SessionEventKindViewRunFailed)
        return kind["message"]
    return None


async def native_platform_types(client: AsyncLightspeedClient) -> None:
    assert_type(await client.call("platform/organizations/list"), Platform.PlatformOrganizationsListResult)
    assert_type(await client.platform.list_organizations(), Platform.PlatformOrganizationsListResult)
    assert_type(await client.platform.read_organization({"organizationId": "organization"}), Platform.PlatformOrganizationsReadResult)
    assert_type(await client.call("platform/universes/create", {"organizationId": "organization", "name": "Experiment"}), Platform.PlatformUniversesCreateResult)
    assert_type(await client.platform.create_universe({"organizationId": "organization", "name": "Experiment"}), Platform.PlatformUniversesCreateResult)
    assert_type(await client.platform.create_personal_api_key({"name": "Automation"}), Platform.PlatformCredentialsCreateResult)
    assert_type(client.organization_id, str | None)
    assert_type(await AsyncLightspeedClient.from_connection("work", organization="organization"), AsyncLightspeedClient)


async def typed_discovery(client: AsyncLightspeedClient) -> None:
    info = await client.initialize()
    assert_type(info, ClientConnectionResponse)
    assert_type(await client.call("initialize"), ClientConnectionResponse)
    if info["runtime"] == "platform":
        assert_type(info["platform"], Platform.PlatformConnectionDetails)


async def complete_named_helper_types(
    client: AsyncLightspeedClient,
    source: Api.RunStartSource,
    bot: Api.BotInput,
    provider: Api.CredentialProviderConfigInput,
    stream: AsyncIterator[bytes],
) -> None:
    assert_type(await client.list_universes(metadata={"team": "build"}), Api.DeploymentUniverseListResponse)
    assert_type(await client.create_universe("uuid", protected=True), Api.DeploymentUniverseCreateResponse)
    assert_type(await client.put_universe_configuration("uuid", protected=False, display_name=None), Api.DeploymentUniverseReadResponse)
    assert_type(await client.put_universe_expiry("uuid", expires_at_ms=None), Api.DeploymentUniverseReadResponse)
    assert_type(await client.put_environment_provider("provider", controller_endpoint="https://controller.example"), Api.DeploymentEnvironmentProviderPutResponse)
    assert_type(await client.list_environment_registration_keys(), Api.EnvironmentRegistrationKeyListResponse)
    assert_type(await client.put_credential_grant("grant", token="token", expires_at_ms=None), Api.CredentialGrantPutResponse)
    assert_type(await client.put_credential_provider("provider", config=provider), Api.CredentialProviderPutResponse)
    assert_type(await client.create_bot(bot=bot), Api.BotCreateResponse)
    assert_type(await client.read_bot_event("bot", seq=4), Api.BotEventReadResponse)
    assert_type(await client.start_run("session", source, notify_on_terminal={"token": "terminal"}), Api.RunStartResponse)
    assert_type(await client.read_events("session", direction="backward", before={"seq": 5}), Api.SessionEventsReadResponse)
    assert_type(await client.upload_blob("sha256:blob", stream, content_length=3), Api.BlobPutResult)
    assert_type(await client.read_workspace_file("workspace", "/file"), bytes)
    assert_type(await client.read_blob_bytes("sha256:blob"), bytes)
    assert_type(client.watch(), AsyncIterator[Api.AgentNotification])
