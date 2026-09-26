import type { DeploymentChannelAccountView } from "@lightspeed-ai/agent-client";

export const UNIVERSE_A = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
export const UNIVERSE_B = "00000000-0000-0000-0000-000000000001";

export function account(
  overrides: Partial<DeploymentChannelAccountView> & Pick<DeploymentChannelAccountView, "accountId">,
): DeploymentChannelAccountView {
  return {
    universeId: UNIVERSE_A,
    provider: "telegram",
    providerAccountId: `${overrides.accountId}_bot`,
    displayName: overrides.accountId,
    credentialGrantId: `grant-${overrides.accountId}`,
    enabled: true,
    revision: 1,
    settings: {},
    createdAtMs: 1_700_000_000_000,
    updatedAtMs: 1_700_000_000_000,
    ...overrides,
  };
}

/** A fake JSON-RPC transport: answers `method` with `result`, records every call. */
export function fakeRpc(
  handler: (method: string, params: unknown, headers: Headers) => unknown,
): { fetch: typeof fetch; calls: Array<{ method: string; params: unknown; headers: Headers }> } {
  const calls: Array<{ method: string; params: unknown; headers: Headers }> = [];
  const fetch: typeof globalThis.fetch = async (_input, init) => {
    const body = JSON.parse(String(init?.body)) as { id: unknown; method: string; params: unknown };
    const headers = new Headers(init?.headers);
    calls.push({ method: body.method, params: body.params, headers });
    const result = handler(body.method, body.params, headers);
    if (result instanceof Error) {
      return Response.json({ id: body.id, error: { code: -32010, message: result.message } });
    }
    return Response.json({ id: body.id, result: { result } });
  };
  return { fetch, calls };
}
