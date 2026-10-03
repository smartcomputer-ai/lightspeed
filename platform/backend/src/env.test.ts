import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { loadEnv } from "./env.js";

beforeEach(() => {
  for (const name of Object.keys(process.env).filter((name) => name.startsWith("LIGHTSPEED_PLATFORM_OIDC_"))) {
    vi.stubEnv(name, "");
  }
  for (const name of [
    "LIGHTSPEED_PLATFORM_DATABASE_URL",
    "LIGHTSPEED_PLATFORM_AUTH_SECRET",
    "LIGHTSPEED_PLATFORM_DEV_SEED",
    "LIGHTSPEED_PLATFORM_BASE_URL",
    "LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS",
    "LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID",
    "LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET",
    "LIGHTSPEED_PLATFORM_PASSWORD_SIGN_IN",
    "LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_ALLOW_PRIVATE_NETWORK",
    "LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER",
  ]) {
    vi.stubEnv(name, "");
  }
});

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("platform environment", () => {
  test("loads the Lightspeed platform names", () => {
    vi.stubEnv("LIGHTSPEED_PLATFORM_DATABASE_URL", "postgres://platform");
    vi.stubEnv("LIGHTSPEED_PLATFORM_AUTH_SECRET", "platform-secret");
    vi.stubEnv("LIGHTSPEED_PLATFORM_BASE_URL", "https://platform.example");
    vi.stubEnv(
      "LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS",
      "https://app.example, https://admin.example",
    );

    const env = loadEnv();

    expect(env.databaseUrl).toBe("postgres://platform");
    expect(env.authSecret).toBe("platform-secret");
    expect(env.baseUrl).toBe("https://platform.example");
    expect(env.trustedOrigins).toEqual(["https://app.example", "https://admin.example"]);
    expect(env.configuratorMcpAllowPrivateNetwork).toBe(false);
    expect(env.devSeed).toBe(false);
    expect(env.configuratorMcpInternalTrustedHeader).toBe(false);
  });

  test("loads the Configurator MCP private-network opt-in", () => {
    vi.stubEnv("LIGHTSPEED_PLATFORM_DATABASE_URL", "postgres://platform");
    vi.stubEnv("LIGHTSPEED_PLATFORM_AUTH_SECRET", "platform-secret");
    vi.stubEnv("LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_ALLOW_PRIVATE_NETWORK", "true");

    expect(loadEnv().configuratorMcpAllowPrivateNetwork).toBe(true);
  });

  test("rejects an invalid Configurator MCP private-network opt-in", () => {
    vi.stubEnv("LIGHTSPEED_PLATFORM_DATABASE_URL", "postgres://platform");
    vi.stubEnv("LIGHTSPEED_PLATFORM_AUTH_SECRET", "platform-secret");
    vi.stubEnv("LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_ALLOW_PRIVATE_NETWORK", "yes");

    expect(() => loadEnv()).toThrowError(
      "LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_ALLOW_PRIVATE_NETWORK must be true or false",
    );
  });

  test("rejects the retired Configurator trusted-header path", () => {
    vi.stubEnv("LIGHTSPEED_PLATFORM_DATABASE_URL", "postgres://platform");
    vi.stubEnv("LIGHTSPEED_PLATFORM_AUTH_SECRET", "platform-secret");
    vi.stubEnv("LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER", "true");

    expect(() => loadEnv()).toThrow(/retired/);
  });

  test("requires the Lightspeed platform names", () => {
    expect(() => loadEnv()).toThrowError(
      "Missing required environment variable LIGHTSPEED_PLATFORM_DATABASE_URL",
    );
  });
});

describe("company sign-in configuration", () => {
  const configure = () => {
    vi.stubEnv("LIGHTSPEED_PLATFORM_DATABASE_URL", "postgres://platform");
    vi.stubEnv("LIGHTSPEED_PLATFORM_AUTH_SECRET", "platform-secret");
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_ISSUER", "https://identity.example.test");
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_CLIENT_ID", "lightspeed");
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_CLIENT_SECRET", "synthetic-secret");
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_USER_GROUP", "lightspeed-users");
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_ADMIN_GROUP", "lightspeed-admins");
  };
  test("selects SSO with bounded sessions and emergency-only passwords", () => {
    configure();
    expect(loadEnv()).toMatchObject({ passwordSignIn: "break-glass", oidc: { claimsToken: "id", sessionMaxAgeSeconds: 28800, scopes: ["openid", "profile", "email"], autoSignIn: true } });
  });
  test.each([true, false])("loads automatic sign-in as %s", (enabled) => {
    configure();
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_AUTO_SIGN_IN", String(enabled));
    expect(loadEnv().oidc?.autoSignIn).toBe(enabled);
  });
  test("rejects invalid automatic sign-in configuration", () => {
    configure();
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_AUTO_SIGN_IN", "yes");
    expect(() => loadEnv()).toThrow("LIGHTSPEED_PLATFORM_OIDC_AUTO_SIGN_IN must be true or false");
  });
  test("fails on partial configuration instead of restoring password access", () => {
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_CLIENT_ID", "lightspeed");
    expect(() => loadEnv()).toThrow(/OIDC_ISSUER/);
  });
  test.each([
    ["USER_GROUP", "lightspeed-admins"], ["CLAIMS_TOKEN", "anything"], ["SESSION_MAX_AGE_SECONDS", "0"],
    ["SCOPES", "openid offline_access"], ["SCOPES", "email"], ["DISCOVERY_URL", "http://identity.example.test/config"],
  ])("rejects unsafe %s configuration", (name, value) => {
    configure();
    vi.stubEnv(`LIGHTSPEED_PLATFORM_OIDC_${name}`, value);
    expect(() => loadEnv()).toThrow();
  });
  test("requires an access-token audience and permits AD FS's separate discovery URL", () => {
    configure();
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_CLAIMS_TOKEN", "access");
    expect(() => loadEnv()).toThrow(/AUDIENCE/);
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_AUDIENCE", "urn:lightspeed");
    vi.stubEnv("LIGHTSPEED_PLATFORM_OIDC_DISCOVERY_URL", "https://identity.example.test/adfs/.well-known/openid-configuration");
    expect(loadEnv().oidc?.audience).toBe("urn:lightspeed");
  });
  test("refuses ordinary password mode when OIDC is configured", () => {
    configure();
    vi.stubEnv("LIGHTSPEED_PLATFORM_PASSWORD_SIGN_IN", "local");
    expect(() => loadEnv()).toThrow(/PASSWORD_SIGN_IN/);
  });
});
