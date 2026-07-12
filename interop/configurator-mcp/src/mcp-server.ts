import type { LightspeedClient, MethodParams } from "@lightspeed/agent-client";
import AjvModule, { type ErrorObject, type ValidateFunction } from "ajv";
import addFormatsModule from "ajv-formats";
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import {
  CallToolRequestSchema,
  ErrorCode,
  ListToolsRequestSchema,
  McpError,
  type CallToolResult,
  type Tool,
} from "@modelcontextprotocol/sdk/types.js";
import { GENERATED_TOOLS } from "./generated/tools.js";
import type { RequestAuthContext } from "./request-auth.js";
import type { GeneratedToolDescriptor } from "./tool-descriptor.js";
import { failedToolResult, successfulToolResult } from "./tool-result.js";
import { requestSignal, type UpstreamClientFactory } from "./upstream-client.js";

export interface ToolRegistry {
  readonly tools: readonly Tool[];
  createServer(auth: RequestAuthContext): Server;
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
    createServer(auth) {
      const server = new Server(
        { name: "lightspeed-configurator", version: "0.0.0" },
        {
          capabilities: { tools: {} },
          instructions:
            "These tools expose the complete universe-scoped Lightspeed API. " +
            "Revision-guarded puts require the caller to read the current document first.",
        },
      );

      server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: [...tools] }));
      server.setRequestHandler(
        CallToolRequestSchema,
        async (request, extra): Promise<CallToolResult> => {
          const descriptor = byName.get(request.params.name);
          if (!descriptor) {
            throw new McpError(ErrorCode.InvalidParams, `unknown tool: ${request.params.name}`);
          }
          const args = request.params.arguments ?? {};
          const validate = validators.get(descriptor.name);
          if (!validate || !validate(args)) {
            throw new McpError(
              ErrorCode.InvalidParams,
              `invalid arguments for ${descriptor.name}: ${formatValidationErrors(validate?.errors)}`,
            );
          }
          try {
            const client = factory(auth);
            const outcome = await callLightspeed(
              client,
              descriptor,
              args,
              requestSignal(extra.signal, upstreamTimeoutMs),
            );
            return successfulToolResult(outcome);
          } catch (error) {
            if (extra.signal.aborted) {
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
