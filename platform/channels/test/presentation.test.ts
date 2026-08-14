import { describe, expect, it } from "vitest";
import { renderTelegramHtml } from "../src/presentation/telegram.js";
import { splitMessageText } from "../src/presentation/text.js";
import { renderWhatsAppText } from "../src/presentation/whatsapp.js";

describe("channel presentation", () => {
  it("renders Telegram-safe HTML without trusting raw model HTML", () => {
    expect(renderTelegramHtml("**bold** and `<x>`")).toBe(
      "<b>bold</b> and <code>&lt;x&gt;</code>",
    );
    expect(renderTelegramHtml("<b>raw</b>")).toBe("&lt;b&gt;raw&lt;/b&gt;");
  });

  it("splits on useful boundaries and enforces receipt bounds", () => {
    expect(splitMessageText("alpha beta gamma", 11)).toEqual(["alpha beta", "gamma"]);
    expect(() => splitMessageText("abcdefgh", 2, 2)).toThrow("2-chunk");
  });

  it("renders WhatsApp-native inline formatting", () => {
    expect(renderWhatsAppText("## Title\n\n**bold** and [docs](https://example.com)")).toBe(
      "*Title*\n\n*bold* and docs (https://example.com)",
    );
  });
});
