import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { loadEnv } from "./env.js";

beforeEach(() => {
  for (const name of [
    "LIGHTSPEED_PLATFORM_DATABASE_URL",
    "LIGHTSPEED_PLATFORM_AUTH_SECRET",
    "LIGHTSPEED_PLATFORM_BASE_URL",
    "LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID",
    "LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET",
    "LSBOT_DATABASE_URL",
    "LSBOT_AUTH_SECRET",
    "LSBOT_BASE_URL",
    "LSBOT_GITHUB_CLIENT_ID",
    "LSBOT_GITHUB_CLIENT_SECRET",
  ]) {
    vi.stubEnv(name, "");
  }
});

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("platform environment compatibility", () => {
  test("prefers the Lightspeed platform names", () => {
    vi.stubEnv("LIGHTSPEED_PLATFORM_DATABASE_URL", "postgres://platform");
    vi.stubEnv("LIGHTSPEED_PLATFORM_AUTH_SECRET", "platform-secret");
    vi.stubEnv("LIGHTSPEED_PLATFORM_BASE_URL", "https://platform.example");

    const env = loadEnv();

    expect(env.databaseUrl).toBe("postgres://platform");
    expect(env.authSecret).toBe("platform-secret");
    expect(env.baseUrl).toBe("https://platform.example");
  });

  test("accepts legacy deployment names during the compatibility window", () => {
    vi.stubEnv("LSBOT_DATABASE_URL", "postgres://legacy");
    vi.stubEnv("LSBOT_AUTH_SECRET", "legacy-secret");

    const env = loadEnv();

    expect(env.databaseUrl).toBe("postgres://legacy");
    expect(env.authSecret).toBe("legacy-secret");
  });

  test("rejects conflicting new and legacy values", () => {
    vi.stubEnv("LIGHTSPEED_PLATFORM_DATABASE_URL", "postgres://platform");
    vi.stubEnv("LSBOT_DATABASE_URL", "postgres://legacy");
    vi.stubEnv("LIGHTSPEED_PLATFORM_AUTH_SECRET", "platform-secret");

    expect(() => loadEnv()).toThrowError(
      "Conflicting environment variables LIGHTSPEED_PLATFORM_DATABASE_URL and LSBOT_DATABASE_URL",
    );
  });
});
