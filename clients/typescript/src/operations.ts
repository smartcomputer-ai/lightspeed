import type { OperationMap } from "./generated/operations.js";
import type { ConnectionResponse } from "./generated/types.js";
import type { PlatformConnectionDetails } from "./generated/platform-types.js";
import type { PlatformOperationMap } from "./generated/platform.js";

/** The client accepts both catalogs. Runtime server bindings keep their own narrower IDs. */
export type ClientConnectionResponse =
  | (Omit<ConnectionResponse, "runtime"> & { runtime: "runtime" })
  | (Omit<ConnectionResponse, "runtime"> & { runtime: "platform"; platform: PlatformConnectionDetails });
export type ClientOperationMap = Omit<OperationMap, "initialize"> & PlatformOperationMap & {
  initialize: { params: OperationMap["initialize"]["params"]; result: ClientConnectionResponse };
};
export type ClientOperationId = keyof ClientOperationMap;
export type ClientOperationParams<K extends ClientOperationId> = ClientOperationMap[K]["params"];
export type ClientOperationResult<K extends ClientOperationId> = ClientOperationMap[K]["result"];
