import { Marked, type RendererObject, type Tokens } from "marked";

/// Model output is markdown, but chat channels are not: Telegram renders an
/// explicit HTML subset (parse_mode HTML — the Markdown parse modes 400 on
/// any unbalanced marker), WhatsApp has its own inline syntax. These
/// renderers convert per channel; constructs with no channel equivalent
/// (headings, lists, tables) downconvert to plain text shapes.

function escapeHtml(text: string): string {
  return text
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function listText(
  parse: (tokens: Tokens.Generic[]) => string,
  token: Tokens.List,
): string {
  const start = typeof token.start === "number" ? token.start : 1;
  const lines = token.items.map((item, index) => {
    const marker = token.ordered ? `${start + index}. ` : "• ";
    const checkbox = item.task ? (item.checked ? "☑ " : "☐ ") : "";
    const body = parse(item.tokens as Tokens.Generic[]).trim().replaceAll("\n\n", "\n");
    return `${marker}${checkbox}${body}`;
  });
  return `${lines.join("\n")}\n\n`;
}

const telegramRenderer: RendererObject = {
  // Block constructs.
  paragraph(token) {
    return `${this.parser.parseInline(token.tokens)}\n\n`;
  },
  heading(token) {
    return `<b>${this.parser.parseInline(token.tokens)}</b>\n\n`;
  },
  code(token) {
    const lang = token.lang?.trim().split(/\s+/)[0];
    const classAttr = lang ? ` class="language-${escapeHtml(lang)}"` : "";
    return `<pre><code${classAttr}>${escapeHtml(token.text)}</code></pre>\n\n`;
  },
  blockquote(token) {
    return `<blockquote>${this.parser.parse(token.tokens).trim()}</blockquote>\n\n`;
  },
  list(token) {
    return listText((tokens) => this.parser.parse(tokens), token);
  },
  table(token) {
    // No table support in chat; monospace keeps the columns readable.
    return `<pre>${escapeHtml(token.raw.trim())}</pre>\n\n`;
  },
  hr() {
    return "———\n\n";
  },
  html(token) {
    // Raw HTML in model output is untrusted text, not markup.
    return escapeHtml(token.text);
  },
  space() {
    return "";
  },
  // Inline constructs.
  strong(token) {
    return `<b>${this.parser.parseInline(token.tokens)}</b>`;
  },
  em(token) {
    return `<i>${this.parser.parseInline(token.tokens)}</i>`;
  },
  del(token) {
    return `<s>${this.parser.parseInline(token.tokens)}</s>`;
  },
  codespan(token) {
    return `<code>${escapeHtml(token.text)}</code>`;
  },
  link(token) {
    return `<a href="${escapeHtml(token.href)}">${this.parser.parseInline(token.tokens)}</a>`;
  },
  image(token) {
    return escapeHtml(token.text || token.href);
  },
  text(token) {
    return "tokens" in token && token.tokens
      ? this.parser.parseInline(token.tokens)
      : escapeHtml(token.text);
  },
  br() {
    return "\n";
  },
};

const whatsappRenderer: RendererObject = {
  paragraph(token) {
    return `${this.parser.parseInline(token.tokens)}\n\n`;
  },
  heading(token) {
    return `*${this.parser.parseInline(token.tokens)}*\n\n`;
  },
  code(token) {
    return `\`\`\`\n${token.text}\n\`\`\`\n\n`;
  },
  blockquote(token) {
    const body = this.parser.parse(token.tokens).trim();
    return `${body
      .split("\n")
      .map((line) => `> ${line}`)
      .join("\n")}\n\n`;
  },
  list(token) {
    return listText((tokens) => this.parser.parse(tokens), token);
  },
  table(token) {
    return `\`\`\`\n${token.raw.trim()}\n\`\`\`\n\n`;
  },
  hr() {
    return "———\n\n";
  },
  html(token) {
    return token.text;
  },
  space() {
    return "";
  },
  strong(token) {
    return `*${this.parser.parseInline(token.tokens)}*`;
  },
  em(token) {
    return `_${this.parser.parseInline(token.tokens)}_`;
  },
  del(token) {
    return `~${this.parser.parseInline(token.tokens)}~`;
  },
  codespan(token) {
    return `\`${token.text}\``;
  },
  link(token) {
    const label = this.parser.parseInline(token.tokens);
    return label && label !== token.href ? `${label} (${token.href})` : token.href;
  },
  image(token) {
    return token.text || token.href;
  },
  text(token) {
    return "tokens" in token && token.tokens
      ? this.parser.parseInline(token.tokens)
      : token.text;
  },
  br() {
    return "\n";
  },
};

const telegramMarked = new Marked({ gfm: true, renderer: telegramRenderer });
const whatsappMarked = new Marked({ gfm: true, renderer: whatsappRenderer });

function render(engine: Marked, markdown: string): string {
  const output = engine.parse(markdown, { async: false });
  return output.replace(/\n{3,}/g, "\n\n").trim();
}

/// Markdown to the Telegram parse_mode=HTML subset.
export function renderTelegramHtml(markdown: string): string {
  return render(telegramMarked, markdown);
}

/// Markdown to WhatsApp's native inline syntax.
export function renderWhatsAppText(markdown: string): string {
  return render(whatsappMarked, markdown);
}
