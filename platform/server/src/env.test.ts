import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { loadEnv } from "./env.js";

beforeEach(() => {
  for (const name of [
    "LIGHTSPEED_PLATFORM_DATABASE_URL",
    "LIGHTSPEED_PLATFORM_AUTH_SECRET",
    "LIGHTSPEED_PLATFORM_BASE_URL",
    "LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS",
    "LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID",
    "LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET",
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
