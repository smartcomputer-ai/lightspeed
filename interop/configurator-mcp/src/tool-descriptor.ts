import type { Method } from "@lightspeed/agent-client";

export interface JsonSchema {
  [key: string]: unknown;
}

export interface GeneratedToolDescriptor {
  name: string;
  method: Method;
  summary: string;
  description: string;
  paramsType: string;
  resultType: string;
  inputSchema: JsonSchema;
}
