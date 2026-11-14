import { describe, expect, it, vi } from "vitest";

// The module creates the sign-in client on import; only the reader is under test.
vi.mock("better-auth/react", () => ({ createAuthClient: () => ({}) }));
vi.mock("better-auth/client/plugins", () => ({ adminClient: () => ({}) }));

import { readLoginConfig } from "./auth";

const message = (response: Response) => readLoginConfig(response).then(
  () => { throw new Error("expected the answer to be refused"); },
  (error: unknown) => (error as Error).message,
);

describe("readLoginConfig", () => {
  it("returns the settings", async () => {
    const config = { sso: false, autoSignIn: false, providerId: null, password: "local" };
    await expect(readLoginConfig(Response.json(config))).resolves.toEqual(config);
  });

  it("names the status and text of a failed answer that is not JSON", async () => {
    expect(await message(new Response("upstream request timeout", { status: 504 })))
      .toBe("Sign-in settings are unavailable (504: upstream request timeout). Try again.");
  });

  it("names a successful answer that is not JSON", async () => {
    expect(await message(new Response("<!doctype html><title>Portal</title>", { status: 200 })))
      .toBe("Sign-in settings are unavailable (200: <!doctype html><title>Portal</title>). Try again.");
  });

  it.each([
    ["a JSON error", Response.json({ error: "unavailable" }, { status: 503 })],
    ["a failed answer without a body", new Response(null, { status: 502 })],
    ["a successful answer without a body", new Response(null, { status: 204 })],
  ])("keeps the plain message for %s", async (_name, response) => {
    expect(await message(response)).toBe("Sign-in settings are unavailable. Try again.");
  });
});
