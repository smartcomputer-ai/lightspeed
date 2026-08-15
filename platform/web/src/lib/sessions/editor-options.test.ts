import { describe, expect, it } from "vitest";
import type { EnvironmentProviderBinding } from "@/api";
import { environmentProviderOptions } from "./editor-options";

const binding = (
  bindingId: string,
  providerId: string,
  status: EnvironmentProviderBinding["status"],
): EnvironmentProviderBinding => ({
  bindingId,
  providerId,
  status,
  revision: 1,
  createdAtMs: 1,
  updatedAtMs: 1,
});

describe("environment provider options", () => {
  it("includes each enabled physical provider once", () => {
    expect(environmentProviderOptions([
      binding("disabled-a", "provider-a", "disabled"),
      binding("enabled-a-2", "provider-a", "enabled"),
      binding("enabled-b", "provider-b", "enabled"),
      binding("enabled-a-1", "provider-a", "enabled"),
    ])).toEqual([
      { providerId: "provider-a", displayName: undefined },
      { providerId: "provider-b", displayName: undefined },
    ]);
  });
});
