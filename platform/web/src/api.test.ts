import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError, api } from "./api";

function answer(body: BodyInit | null, init: ResponseInit) {
  vi.stubGlobal("fetch", vi.fn(async () => new Response(body, init)));
}

async function failure(): Promise<ApiError> {
  const error: unknown = await api("GET", "/api/v1/universes").catch((caught: unknown) => caught);
  expect(error).toBeInstanceOf(ApiError);
  return error as ApiError;
}

afterEach(() => vi.unstubAllGlobals());

describe("api", () => {
  it("reports the status and text of a failed answer that is not JSON", async () => {
    answer("upstream request timeout\n", { status: 504, headers: { "content-type": "text/plain" } });
    const error = await failure();
    expect(error.status).toBe(504);
    expect(error.message).toBe("504: upstream request timeout");
    expect(error.body).toBe("upstream request timeout");
  });

  it("shortens an HTML error page to one line", async () => {
    const page = `<html>\n  <head><title>502 Bad Gateway</title></head>\n  <body>\n${"    <p>The server is temporarily unable to service your request.</p>\n".repeat(40)}  </body>\n</html>\n`;
    answer(page, { status: 502, headers: { "content-type": "text/html" } });
    const error = await failure();
    expect(error.status).toBe(502);
    expect(error.message.startsWith("502: <html> <head><title>502 Bad Gateway</title></head> <body> <p>The server")).toBe(true);
    expect(error.message.endsWith("…")).toBe(true);
    expect(error.message).not.toContain("\n");
    // The status prefix, the excerpt and the mark that it was cut.
    expect(error.message).toHaveLength("502: ".length + 300 + 1);
  });

  it("does not trust a JSON content type on a body that is not JSON", async () => {
    answer("Service Unavailable", { status: 503, headers: { "content-type": "application/json" } });
    expect((await failure()).message).toBe("503: Service Unavailable");
  });

  it("keeps the generic message for a failed answer without a body", async () => {
    answer(null, { status: 502 });
    const error = await failure();
    expect(error.status).toBe(502);
    expect(error.body).toBeNull();
    expect(error.message).toBe("API error 502");
  });

  it("keeps a JSON error body and the message derived from it", async () => {
    const body = { error: "invalid request", issues: [{ path: ["name"], message: "Required" }] };
    answer(JSON.stringify(body), { status: 400, headers: { "content-type": "application/json" } });
    const error = await failure();
    expect(error.status).toBe(400);
    expect(error.body).toEqual(body);
    expect(error.message).toBe("invalid request — name: Required");
  });

  it("reads a JSON error body whatever its content type says", async () => {
    answer(JSON.stringify({ error: "session is not open", kind: "operationPending" }), { status: 409 });
    const error = await failure();
    expect(error.body).toEqual({ error: "session is not open", kind: "operationPending" });
    expect(error.message).toBe("session is not open");
  });

  it("returns the parsed JSON of a successful answer", async () => {
    answer(JSON.stringify({ universes: [] }), { status: 200, headers: { "content-type": "application/json" } });
    await expect(api("GET", "/api/v1/universes")).resolves.toEqual({ universes: [] });
  });

  it("refuses a successful answer that is not JSON, naming its status and text", async () => {
    answer("<!doctype html>\n<html><head><title>Sign in</title></head></html>", { status: 200, headers: { "content-type": "text/html" } });
    const error = await failure();
    expect(error.status).toBe(200);
    expect(error.message).toBe("Unexpected response (200), not JSON: <!doctype html> <html><head><title>Sign in</title></head></html>");
  });

  it.each([
    ["no content", null, 204],
    ["an empty body", "", 200],
    ["only whitespace", "\n", 200],
  ])("returns null for a successful answer with %s", async (_name, body, status) => {
    answer(body, { status });
    await expect(api("DELETE", "/api/v1/universes/u/sessions/s")).resolves.toBeNull();
  });
});
