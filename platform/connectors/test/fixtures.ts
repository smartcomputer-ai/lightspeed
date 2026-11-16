import { decodeHttpRequest } from "../../test/http-fixture.js";
import type { DeploymentChannelAccountView } from "@lightspeed-ai/sdk";

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

/** A real HTTP-shaped runtime fixture, recording logical operation parameters. */
export function fakeHttp(
  handler: (method: string, params: unknown, headers: Headers) => unknown,
) {
  const calls: Array<ReturnType<typeof decodeHttpRequest>> = [];
  const fetch: typeof globalThis.fetch = async (input, init) => {
    const request = decodeHttpRequest(input, init);
    calls.push(request);
    const result = handler(request.method, request.params, request.headers);
    if (result instanceof Error) return Response.json({ type: "about:blank", title: "Rejected", detail: result.message, status: 409, kind: "rejected" }, { status: 409, headers: { "content-type": "application/problem+json" } });
    return Response.json(result);
  };
  return { fetch, calls };
}
