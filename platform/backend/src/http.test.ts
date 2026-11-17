import { expect, it, vi } from "vitest";
import { Hono } from "hono";
import { HTTPException } from "hono/http-exception";
import { AuthorizationRefusal } from "./auth-context.js";
import { withApiErrors } from "./http.js";

it("preserves authorization and transaction refusal status for the app error handler", async () => {
  for (const error of [new AuthorizationRefusal(403, "Account is disabled"), new HTTPException(409, { message: "Installation was superseded" })]) {
    const app = new Hono();
    app.onError((failure, c) => failure instanceof HTTPException ? c.json({ error: failure.message }, failure.status) : c.json({ error: "unexpected" }, 500));
    app.get("/", c => withApiErrors(c, async () => { throw error; }));
    const response = await app.request("/");
    expect(response.status).toBe(error.status);
    expect(await response.json()).toEqual({ error: error.message });
  }
});

it("keeps untyped database diagnostics out of API responses", async () => {
  const log = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    const app = new Hono();
    app.get("/", c => withApiErrors(c, async () => { throw new Error("sensitive SQL parameters"); }));
    const response = await app.request("/");
    expect(response.status).toBe(502);
    expect(await response.json()).toEqual({ error: "internal error" });
  } finally { log.mockRestore(); }
});
