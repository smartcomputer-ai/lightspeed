import { mcpDiscoveryFailureAction } from "@/lib/mcp/tool-discovery";
import { describe, expect, it } from "vitest";
import {
  isValidMcpUrl,
  mcpAuthPolicyInput,
  mcpAuthKind,
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

  it("builds OAuth policy metadata and defaults the resource to the server URL", () => {
    expect(mcpAuthPolicyInput({
      type: "requiredOAuth",
      serverUrl: "https://mcp.example.com/mcp",
      resource: "",
      scopes: "read, write, read",
      metadataUrl: " https://mcp.example.com/.well-known/oauth-protected-resource ",
      authorizationServer: "",
    })).toEqual({
      type: "requiredOAuth",
      resource: "https://mcp.example.com/mcp",
      scopesDefault: ["read", "write"],
      protectedResourceMetadataUrl:
        "https://mcp.example.com/.well-known/oauth-protected-resource",
    });
  });

  it("reduces wire policies to the three choices people need", () => {
    expect(mcpAuthKind("none")).toBe("none");
    expect(mcpAuthKind("optionalBearer")).toBe("bearer");
    expect(mcpAuthKind("requiredOAuth")).toBe("oauth");
  });

  it("accepts only credential-free HTTP MCP URLs", () => {
    expect(isValidMcpUrl("https://mcp.example.com/mcp")).toBe(true);
    expect(isValidMcpUrl("http://127.0.0.1:9000/mcp")).toBe(true);
    expect(isValidMcpUrl("ftp://mcp.example.com/mcp")).toBe(false);
    expect(isValidMcpUrl("https://user:secret@mcp.example.com/mcp")).toBe(false);
    expect(isValidMcpUrl("https://mcp.example.com/mcp#tools")).toBe(false);
  });

  it("turns stable discovery failures into actionable guidance", () => {
    expect(mcpDiscoveryFailureAction("forbidden")).toContain("scopes");
    expect(mcpDiscoveryFailureAction("grantAudienceMismatch")).toContain("exact server");
    expect(mcpDiscoveryFailureAction("responseTooLarge")).toContain("safe discovery limits");
  });
});
