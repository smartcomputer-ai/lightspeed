import { afterEach, expect, it, vi } from "vitest";
import { api } from "./api";
afterEach(() => vi.unstubAllGlobals());
it.each([true, false])(
  "retains response-specific privileged provenance (%s)",
  async (privileged) => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json(
          { id: "session" },
          {
            headers: privileged
              ? { "x-lightspeed-privileged-read": "true" }
              : {},
          },
        ),
      ),
    );
    expect(await api("GET", "/api/v1/session")).toEqual(
      privileged ? { id: "session", privilegedRead: true } : { id: "session" },
    );
  },
);
