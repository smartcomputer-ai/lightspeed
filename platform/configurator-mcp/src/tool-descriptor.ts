import type { OperationId, MethodGroup } from "@lightspeed-ai/sdk";

export interface JsonSchema {
  [key: string]: unknown;
}

export interface GeneratedToolDescriptor {
  name: string;
  method: OperationId;
  /// The API family published by the runtime contract.
  group: MethodGroup;
  summary: string;
  description: string;
  paramsType: string;
  resultType: string;
  inputSchema: JsonSchema;
}
