import { describe, expect, it } from "vitest";
import {
  mcpGrantCompatible,
  mcpServerCredentialError,
  mcpServerStatusForCredential,
} from "./McpServersPage";

describe("MCP server credential ownership", () => {
  it("accepts only grants matching the server authentication policy", () => {
    expect(mcpGrantCompatible("requiredBearer", "staticBearer")).toBe(true);
    expect(mcpGrantCompatible("optionalBearer", "mcpOAuth")).toBe(false);
    expect(mcpGrantCompatible("requiredOAuth", "mcpOAuth")).toBe(true);
    expect(mcpGrantCompatible("optionalOAuth", "staticBearer")).toBe(false);
    expect(mcpGrantCompatible("none", "staticBearer")).toBe(false);
  });

  it("moves required-auth servers through needs-auth-config with their binding", () => {
    expect(mcpServerStatusForCredential("requiredBearer", "active", ""))
      .toBe("needsAuthConfig");
    expect(mcpServerStatusForCredential("requiredOAuth", "needsAuthConfig", "authgrant_1"))
      .toBe("active");
    expect(mcpServerStatusForCredential("none", "needsAuthConfig", ""))
      .toBe("active");
    expect(mcpServerStatusForCredential("requiredBearer", "disabled", "authgrant_1"))
      .toBe("disabled");
  });

  it("rejects credentials on public servers", () => {
    expect(mcpServerCredentialError("none", "authgrant_1")).toContain(
      "cannot have an access credential",
    );
    expect(mcpServerCredentialError("requiredBearer", "")).toBeNull();
  });
});
