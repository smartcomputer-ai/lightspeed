import { expect, it } from "vitest";
import { prepareHttpRequest } from "../src/server.js";

it("partitions nested identifiers without mutating inputs", () => {
  const params = { profile: { profileId: "a/b", displayName: "Example" }, expectedRevision: 2 };
  const request = prepareHttpRequest("profiles/put", params, { endpoint: "https://platform/api/runtime", universe: "u" });
  expect(request.url.toString()).toBe("https://platform/api/runtime/universes/u/profiles/a%2Fb");
  expect(request.method).toBe("PUT");
  expect(request.body).toEqual({ profile: { displayName: "Example" }, expectedRevision: 2 });
  expect(params.profile.profileId).toBe("a/b");
});

it("encodes repeated and JSON query fields with URL-owned targets", () => {
  const request = prepareHttpRequest("universe/watch", { kinds: ["session", "workspace"], visibleTo: "a/b" }, { endpoint: "https://runtime/api/v1", universe: "u" });
  expect(request.url.search).toBe("?kinds=session&kinds=workspace&visibleTo=a%2Fb");
  expect(request.body).toBeUndefined();
  expect(() => prepareHttpRequest("profiles/read", { profileId: "p" }, { endpoint: "https://runtime/api/v1" })).toThrow("universe");
  expect(() => prepareHttpRequest("profiles/read", { profileId: ".." }, { endpoint: "https://runtime/api/v1", universe: "u" })).toThrow("profileId");
});
