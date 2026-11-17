import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { HTTP_CONTRACT_REVISION, HTTP_OPERATIONS } from "../src/server.js";
import { OPERATIONS } from "../src/generated/operations.js";

const contract = JSON.parse(readFileSync(new URL("../../../crates/api/contract/http.json", import.meta.url), "utf8"));

describe("generated HTTP routing contract", () => {
  it("keeps canonical operation identities available without duplicating route declarations", () => {
    expect(HTTP_CONTRACT_REVISION).toBe(contract.contractRevision);
    for (const id of OPERATIONS) expect(HTTP_OPERATIONS).toHaveProperty(id);
    expect(Object.keys(HTTP_OPERATIONS).sort()).toEqual(
      contract.operations.map((operation: { operationId: string }) => operation.operationId).sort(),
    );
    for (const operation of contract.operations) {
      const descriptor = HTTP_OPERATIONS[operation.operationId as keyof typeof HTTP_OPERATIONS];
      expect(descriptor.path).toBe(operation.path);
      expect(descriptor.method).toBe(operation.method);
      expect(descriptor.pathFields).toEqual(operation.pathFields);
      expect(descriptor.queryFields).toEqual(operation.queryFields);
      expect(descriptor.parameters).toEqual(operation.parameters);
      expect(descriptor).not.toHaveProperty("body");
      expect(descriptor).not.toHaveProperty("schemas");
    }
  });

  it("declares streaming operations separately from JSON service calls", () => {
    expect(HTTP_OPERATIONS["universe/watch"].transfer).toBe("sse");
    expect(HTTP_OPERATIONS["blobs/read"].transfer).toBe("binaryDownload");
    expect(HTTP_OPERATIONS["blobs/upload"].transfer).toBe("binaryUpload");
    expect(HTTP_OPERATIONS["blobs/put"].transfer).toBe("json");
    expect(HTTP_OPERATIONS["profiles/read"].method).toBe("GET");
  });
});
