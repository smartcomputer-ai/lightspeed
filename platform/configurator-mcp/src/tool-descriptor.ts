import type { Method, MethodGroup } from "@lightspeed-ai/agent-client";

export interface JsonSchema {
  [key: string]: unknown;
}

export interface GeneratedToolDescriptor {
  name: string;
  method: Method;
  /// The group a key must hold to call the method; the tool is listed only
  /// to callers that hold it.
  group: MethodGroup;
  summary: string;
  description: string;
  paramsType: string;
  resultType: string;
  inputSchema: JsonSchema;
}
