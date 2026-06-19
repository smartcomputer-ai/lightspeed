import { describe, expect, it } from "vitest";
import { cleanChannelMessageId } from "../src/channel_id.js";

describe("cleanChannelMessageId", () => {
  it("accepts raw and envelope-style message ids", () => {
    expect(cleanChannelMessageId("3A4D7AFF1740A13CC8D9")).toBe("3A4D7AFF1740A13CC8D9");
    expect(cleanChannelMessageId("#3A4D7AFF1740A13CC8D9")).toBe("3A4D7AFF1740A13CC8D9");
    expect(cleanChannelMessageId("  #169  ")).toBe("169");
  });
});
