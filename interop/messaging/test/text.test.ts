import { describe, expect, it } from "vitest";
import { extractTriggeredText, splitMessageText } from "../src/text.js";

describe("extractTriggeredText", () => {
  it("extracts slash command text", () => {
    expect(
      extractTriggeredText("/ask summarize this", {
        prefixes: ["/ask", "/forge"],
        requireTrigger: true,
      }),
    ).toBe("summarize this");
  });

  it("extracts bot-addressed slash command text", () => {
    expect(
      extractTriggeredText("/ask@ForgeFamilyBot summarize this", {
        botUsername: "ForgeFamilyBot",
        prefixes: ["/ask"],
        requireTrigger: true,
      }),
    ).toBe("summarize this");
  });

  it("extracts mention-triggered text", () => {
    expect(
      extractTriggeredText("@forge: what changed?", {
        mentionNames: ["forge"],
        prefixes: ["/ask"],
        requireTrigger: true,
      }),
    ).toBe("what changed?");
  });

  it("drops untriggered text when triggers are required", () => {
    expect(
      extractTriggeredText("hello", {
        prefixes: ["/ask"],
        requireTrigger: true,
      }),
    ).toBeNull();
  });

  it("allows untriggered text when triggers are optional", () => {
    expect(
      extractTriggeredText("hello", {
        prefixes: ["/ask"],
        requireTrigger: false,
      }),
    ).toBe("hello");
  });
});

describe("splitMessageText", () => {
  it("splits on a nearby word boundary", () => {
    expect(splitMessageText("alpha beta gamma", 11)).toEqual(["alpha beta", "gamma"]);
  });
});
