import { describe, expect, it } from "vitest";
import { renderTelegramHtml, renderWhatsAppText } from "../src/markdown.js";

describe("renderTelegramHtml", () => {
  it("converts inline formatting to the Telegram HTML subset", () => {
    expect(renderTelegramHtml("**bold** and *italic* and `code` and ~~gone~~")).toBe(
      "<b>bold</b> and <i>italic</i> and <code>code</code> and <s>gone</s>",
    );
  });

  it("escapes HTML special characters in text and code", () => {
    expect(renderTelegramHtml("a < b & c > d")).toBe("a &lt; b &amp; c &gt; d");
    expect(renderTelegramHtml("`<script>`")).toBe("<code>&lt;script&gt;</code>");
  });

  it("treats raw HTML in model output as text", () => {
    expect(renderTelegramHtml("<b>not markup</b>")).toBe("&lt;b&gt;not markup&lt;/b&gt;");
  });

  it("downconverts headings and lists", () => {
    expect(renderTelegramHtml("## Reading order\n\n- first\n- second")).toBe(
      "<b>Reading order</b>\n\n• first\n• second",
    );
    expect(renderTelegramHtml("1. one\n2. two")).toBe("1. one\n2. two");
  });

  it("renders fenced code blocks as pre", () => {
    expect(renderTelegramHtml("```rust\nfn main() {}\n```")).toBe(
      '<pre><code class="language-rust">fn main() {}</code></pre>',
    );
  });

  it("renders links and blockquotes", () => {
    expect(renderTelegramHtml("[docs](https://example.com)")).toBe(
      '<a href="https://example.com">docs</a>',
    );
    expect(renderTelegramHtml("> quoted")).toBe("<blockquote>quoted</blockquote>");
  });

  it("keeps bold across multi-line replies", () => {
    const output = renderTelegramHtml("My take:\n\n- **Start practical**, not systematic.");
    expect(output).toBe("My take:\n\n• <b>Start practical</b>, not systematic.");
  });
});

describe("renderWhatsAppText", () => {
  it("converts markdown to WhatsApp inline syntax", () => {
    expect(renderWhatsAppText("**bold** and *italic* and ~~gone~~")).toBe(
      "*bold* and _italic_ and ~gone~",
    );
  });

  it("bolds headings and keeps bullets plain", () => {
    expect(renderWhatsAppText("# Title\n\n- item")).toBe("*Title*\n\n• item");
  });

  it("renders links as label (url)", () => {
    expect(renderWhatsAppText("[docs](https://example.com)")).toBe("docs (https://example.com)");
    expect(renderWhatsAppText("<https://example.com>")).toBe("https://example.com");
  });

  it("keeps code fences", () => {
    expect(renderWhatsAppText("```\nlet x = 1;\n```")).toBe("```\nlet x = 1;\n```");
  });
});
