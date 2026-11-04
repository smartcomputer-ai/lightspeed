import { describe, expect, it, vi } from "vitest";

import { ApiError } from "@/api";

import { postUntilAccepted } from "./held-request";

const pending = () => new ApiError(409, { error: "held", kind: "operationPending", submissionId: "submission" });

describe("postUntilAccepted", () => {
  it("sends the same request again while the session holds it", async () => {
    const post = vi.fn<() => Promise<string>>()
      .mockRejectedValueOnce(pending())
      .mockRejectedValueOnce(pending())
      .mockResolvedValueOnce("run_1");
    await expect(postUntilAccepted(post, () => true)).resolves.toBe("run_1");
    expect(post).toHaveBeenCalledTimes(3);
  });

  it("stops when nobody is watching", async () => {
    const post = vi.fn<() => Promise<string>>().mockRejectedValue(pending());
    await expect(postUntilAccepted(post, () => false)).resolves.toBeNull();
    expect(post).toHaveBeenCalledTimes(1);
  });

  it("does not retry a real failure", async () => {
    const refused = new ApiError(409, { error: "session is not open" });
    const post = vi.fn<() => Promise<string>>().mockRejectedValue(refused);
    await expect(postUntilAccepted(post, () => true)).rejects.toBe(refused);
    expect(post).toHaveBeenCalledTimes(1);
  });
});
