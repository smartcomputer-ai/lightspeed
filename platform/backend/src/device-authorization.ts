import { eq } from "drizzle-orm";
import { APIError } from "better-auth/api";
import { deviceAuthorization, type DeviceAuthorizationGrant } from "better-auth/plugins/device-authorization";
import { deviceCodeFields, schema, type Db } from "@lightspeed-ai/platform-db";
import type { ServerEnv } from "./env.js";
import type { LoginIdentity } from "./company-identity.js";
import { principalAllowed } from "./auth-access.js";

export const CLI_CLIENT_ID = "lightspeed-cli";
export const DEVICE_AUTH_PATHS = new Set(["/device", "/device/code", "/device/token", "/device/approve", "/device/deny"]);
export function deviceAuthorizationPlugin(db: Db, env: ServerEnv, onRedemption: (identity: LoginIdentity) => void) {
  return deviceAuthorization<DeviceAuthorizationGrant<{}, {}>>({
    expiresIn: "10m", interval: "5s", verificationUri: new URL("device", `${env.baseUrl.replace(/\/$/, "")}/`).href,
    validateClient: clientId => clientId === CLI_CLIENT_ID,
    grant: {
      requestSchemaFields: {}, deviceCodeSchemaFields: deviceCodeFields,
      authorizeRequest: ({ request }) => {
        if (request.client_id !== CLI_CLIENT_ID || request.user_id !== undefined || request.scope !== undefined) {
          throw new APIError("BAD_REQUEST", { error: "invalid_request", error_description: "Only the first-party CLI client is supported; user and scopes are chosen by Platform" });
        }
        return { clientId: CLI_CLIENT_ID, deviceCodeFields: {} };
      },
      assertSessionRedemption: async ({ deviceCode }) => {
        // The plugin still owns pending/denied/expiry/polling/atomic consumption.
        // Once approved, the admission snapshot must survive until redemption.
        if (deviceCode.status !== "approved") return;
        const [user] = typeof deviceCode.userId === "string" ? await db.select().from(schema.user).where(eq(schema.user.id, deviceCode.userId)) : [];
        if (!user || !principalAllowed(user, env) || deviceCode.approvalAccessVersion !== user.accessVersion) {
          throw new APIError("BAD_REQUEST", { error: "access_denied", error_description: "Account access changed; sign in and authorize the CLI again" });
        }
        onRedemption({ userId: user.id, accessVersion: user.accessVersion, source: user.identitySource === "company" ? "company" : "password",
          checkedAt: user.identitySource === "company" ? user.providerCheckedAt! : new Date() });
      },
      getVerificationContext: () => undefined,
    },
  });
}

export function deviceProtocolError(error: string, description: string, status = 400) {
  return Response.json({ error, error_description: description }, { status, headers: { "cache-control": "no-store", pragma: "no-cache" } });
}

/** Device endpoints accept one small JSON object, never arbitrary plugin fields. */
export async function readDeviceBody(request: Request): Promise<Record<string, unknown> | null> {
  if (!request.headers.get("content-type")?.split(";")[0]?.trim().toLowerCase().endsWith("/json")) return null;
  const reader = request.body?.getReader();
  if (!reader) return null;
  const chunks: Uint8Array[] = []; let size = 0;
  try {
    for (;;) {
      const next = await reader.read(); if (next.done) break;
      size += next.value.byteLength;
      if (size > 4096) { await reader.cancel(); return null; }
      chunks.push(next.value);
    }
    const bytes = new Uint8Array(size); let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    const parsed: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed as Record<string, unknown> : null;
  } catch { return null; } finally { reader.releaseLock(); }
}
