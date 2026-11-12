import { expect, it } from "vitest";
import { keyIdentifier, keyScopeQuery } from "./api-keys";

it("shows a chosen key id with the secret's prefix, and only the prefix for an allocated id", () => {
  expect(keyIdentifier({ keyId: "platform", keyPrefix: "lsk_ab12cd34" })).toBe("platform · lsk_ab12cd34");
  expect(keyIdentifier({ keyId: "api_key_0123456789abcdef0123456789abcdef", keyPrefix: "lsk_ab12cd34" })).toBe("lsk_ab12cd34");
  // Only the exact allocated form is hidden; a chosen id may resemble it.
  expect(keyIdentifier({ keyId: "api_key_ci", keyPrefix: "lsk_ab12cd34" })).toBe("api_key_ci · lsk_ab12cd34");
});

it("names a universe key's scope and nothing for a deployment key", () => {
  expect(keyScopeQuery({ scope: { kind: "deployment" } })).toBe("");
  expect(keyScopeQuery({ scope: { kind: "universe", universeId: "22222222-2222-4222-8222-222222222222" } }))
    .toBe("?universeId=22222222-2222-4222-8222-222222222222");
});
