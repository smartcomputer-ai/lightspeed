import type { CallerAccess, LightspeedClient, MethodParams } from "@lightspeed-ai/agent-client";
import {
  ProtocolError,
  ProtocolErrorCode,
  Server,
  type CallToolResult,
  type Tool,
} from "@modelcontextprotocol/server";
import AjvModule, { type ErrorObject, type ValidateFunction } from "ajv";
import addFormatsModule from "ajv-formats";
import { GENERATED_TOOLS } from "./generated/tools.js";
import type { RequestAuthContext } from "./request-auth.js";
import type { GeneratedToolDescriptor } from "./tool-descriptor.js";
import { failedToolResult, successfulToolResult } from "./tool-result.js";
import { requestSignal, type UpstreamClientFactory } from "./upstream-client.js";

export interface ToolRegistry {
  readonly tools: readonly Tool[];
  /// A server for one request, offering only the tools whose method group
  /// the caller's key holds. Core still checks every call.
  createServer(auth: RequestAuthContext, caller: CallerAccess): Server;
}

export function createToolRegistry(
  factory: UpstreamClientFactory,
  upstreamTimeoutMs = 60_000,
): ToolRegistry {
  const descriptors = GENERATED_TOOLS;
  const byName = new Map(descriptors.map((descriptor) => [descriptor.name, descriptor]));
  const validators = compileValidators(descriptors);
  const tools = descriptors.map(toMcpTool);

  return {
    tools,
    createServer(auth, caller) {
      const groups = new Set(caller.groups);
      const visible = descriptors.flatMap((descriptor, index) =>
        groups.has(descriptor.group) ? [tools[index]!] : []);
      const server = new Server(
        {
          name: "lightspeed-configurator",
          version: process.env.LIGHTSPEED_RELEASE_VERSION ?? "0.0.0",
        },
        {
          capabilities: { tools: {} },
          instructions:
            "These tools expose the universe-scoped Lightspeed API methods this credential may call. " +
            "Revision-guarded puts require the caller to read the current document first.",
        },
      );

      server.setRequestHandler("tools/list", async () => ({ tools: [...visible] }));
      server.setRequestHandler(
        "tools/call",
        async (request, ctx): Promise<CallToolResult> => {
          const descriptor = byName.get(request.params.name);
          // A tool outside the caller's groups is not offered, so it is
          // unknown to this caller rather than forbidden.
          if (!descriptor || !groups.has(descriptor.group)) {
            throw new ProtocolError(
              ProtocolErrorCode.InvalidParams,
              `unknown tool: ${request.params.name}`,
            );
          }
          const args = request.params.arguments ?? {};
          const validate = validators.get(descriptor.name);
          if (!validate || !validate(args)) {
            throw new ProtocolError(
              ProtocolErrorCode.InvalidParams,
              `invalid arguments for ${descriptor.name}: ${formatValidationErrors(validate?.errors)}`,
            );
          }
          try {
            const client = factory(auth);
            const outcome = await callLightspeed(
              client,
              descriptor,
              args,
              requestSignal(ctx.mcpReq.signal, upstreamTimeoutMs),
            );
            return successfulToolResult(outcome);
          } catch (error) {
            if (ctx.mcpReq.signal.aborted) {
              throw error;
            }
            return failedToolResult(error);
          }
        },
      );
      return server;
    },
  };
}

function toMcpTool(descriptor: GeneratedToolDescriptor): Tool {
  return {
    name: descriptor.name,
    description: `${descriptor.summary}. ${descriptor.description}`,
    inputSchema: descriptor.inputSchema as Tool["inputSchema"],
  };
}

function compileValidators(
  descriptors: readonly GeneratedToolDescriptor[],
): ReadonlyMap<string, ValidateFunction> {
  const Ajv = AjvModule as unknown as typeof import("ajv").default;
  const addFormats = addFormatsModule as unknown as typeof import("ajv-formats").default;
  const ajv = new Ajv({ allErrors: true, strict: false });
  addFormats(ajv);
  for (const format of ["uint", "uint32", "uint64"]) {
    ajv.addFormat(format, { type: "number", validate: (value) => Number.isInteger(value) && value >= 0 });
  }
  for (const format of ["int32", "int64"]) {
    ajv.addFormat(format, { type: "number", validate: Number.isInteger });
  }
  return new Map(
    descriptors.map((descriptor) => [descriptor.name, ajv.compile(descriptor.inputSchema)]),
  );
}

function formatValidationErrors(errors: ErrorObject[] | null | undefined): string {
  if (!errors || errors.length === 0) {
    return "schema validation failed";
  }
  return errors
    .slice(0, 3)
    .map((error) => `${error.instancePath || "/"} ${error.message ?? "is invalid"}`)
    .join("; ");
}

function callLightspeed(
  client: LightspeedClient,
  descriptor: GeneratedToolDescriptor,
  args: unknown,
  signal: AbortSignal,
): Promise<unknown> {
  return client.call(
    descriptor.method,
    args as MethodParams<typeof descriptor.method>,
    { signal },
  );
}
