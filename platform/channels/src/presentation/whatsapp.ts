import { Marked, type RendererObject, type Tokens } from "marked";

const renderer: RendererObject = {
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
    return `${this.parser
      .parse(token.tokens)
      .trim()
      .split("\n")
      .map((line) => `> ${line}`)
      .join("\n")}\n\n`;
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
    return "tokens" in token && token.tokens ? this.parser.parseInline(token.tokens) : token.text;
  },
  br() {
    return "\n";
  },
};

const marked = new Marked({ gfm: true, renderer });

export function renderWhatsAppText(markdown: string): string {
  const output = marked.parse(markdown, { async: false });
  return output.replace(/\n{3,}/g, "\n\n").trim();
}
