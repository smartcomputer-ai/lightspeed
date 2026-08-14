import { Marked, type RendererObject, type Tokens } from "marked";

function escapeHtml(text: string): string {
  return text
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

const renderer: RendererObject = {
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
  list(token: Tokens.List) {
    const start = typeof token.start === "number" ? token.start : 1;
    const lines = token.items.map((item, index) => {
      const marker = token.ordered ? `${start + index}. ` : "• ";
      const checkbox = item.task ? (item.checked ? "☑ " : "☐ ") : "";
      const body = this.parser
        .parse(item.tokens as Tokens.Generic[])
        .trim()
        .replaceAll("\n\n", "\n");
      return `${marker}${checkbox}${body}`;
    });
    return `${lines.join("\n")}\n\n`;
  },
  table(token) {
    return `<pre>${escapeHtml(token.raw.trim())}</pre>\n\n`;
  },
  hr() {
    return "———\n\n";
  },
  html(token) {
    return escapeHtml(token.text);
  },
  space() {
    return "";
  },
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

const marked = new Marked({ gfm: true, renderer });

export function renderTelegramHtml(markdown: string): string {
  const output = marked.parse(markdown, { async: false });
  return output.replace(/\n{3,}/g, "\n\n").trim();
}
