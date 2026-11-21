import { unified } from "unified";
import remarkParse from "remark-parse";
import remarkGfm from "remark-gfm";
import remarkStringify from "remark-stringify";

const markdown = unified().use(remarkParse).use(remarkGfm).use(remarkStringify);
type MarkdownNode = ReturnType<typeof markdown.parse> | ReturnType<typeof markdown.parse>["children"][number];
type Citation = { url: string; title?: string | null };

function portableUrl(value: string): string | null {
  const url = new URL(value, window.location.href);
  return ["http:", "https:", "mailto:"].includes(url.protocol) ? url.href : null;
}

/** Preserve the original Markdown except attachment destinations, which need
 * viewer URLs outside the transcript. Parse links so code examples stay intact. */
export function replyMarkdown(text: string, content: HTMLElement, citations: Citation[] = []): string {
  const references = new Map<string, string>();
  for (const link of content.querySelectorAll<HTMLAnchorElement>("a[data-attachment-reference]")) {
    const url = portableUrl(link.href);
    if (url) references.set(link.dataset.attachmentReference!, url);
  }
  const tree = markdown.parse(text);
  const definitions = new Map<string, string>();
  function collectDefinitions(node: MarkdownNode) {
    if (node.type === "definition" && !definitions.has(node.identifier)) definitions.set(node.identifier, node.url);
    if ("children" in node) node.children.forEach(collectDefinitions);
  }
  collectDefinitions(tree);
  const replacements: Array<{ start: number; end: number; text: string }> = [];
  function visit(node: MarkdownNode) {
    // An image definition must become a link too: the viewer is a page,
    // not an image endpoint that another Markdown renderer could embed.
    if (node.type === "imageReference" && /^(file|media):/.test(definitions.get(node.identifier) ?? "")) {
      const url = references.get(definitions.get(node.identifier)!);
      if (!url) throw new Error("An attachment link is unavailable. Wait for it to load and try again.");
      const start = node.position?.start.offset;
      const end = node.position?.end.offset;
      if (start !== undefined && end !== undefined) replacements.push({ start, end, text: markdown.stringify({
        type: "root", children: [{ type: "link", url, children: [{ type: "text", value: node.alt || "Image" }] }],
      }).trimEnd() });
      return;
    }
    if ((node.type === "link" || node.type === "image" || node.type === "definition")
      && /^(file|media):/.test(node.url)) {
      const url = references.get(node.url);
      if (!url) throw new Error("An attachment link is unavailable. Wait for it to load and try again.");
      const start = node.position?.start.offset;
      const end = node.position?.end.offset;
      if (start !== undefined && end !== undefined) {
        replacements.push({ start, end, text: markdown.stringify({
          type: "root", children: [node.type === "image"
            ? { type: "link", url, title: node.title, children: [{ type: "text", value: node.alt || "Image" }] }
            : { ...node, url }],
        }).trimEnd() });
      }
      return;
    }
    if ("children" in node) node.children.forEach(visit);
  }
  visit(tree);
  for (const replacement of replacements.sort((a, b) => b.start - a.start)) {
    text = text.slice(0, replacement.start) + replacement.text + text.slice(replacement.end);
  }
  if (citations.length) {
    text += "\n\nSources\n\n" + citations.map((citation) => {
      const url = portableUrl(citation.url);
      const label = citation.title || (url ? new URL(url).hostname : citation.url);
      return "- " + markdown.stringify({ type: "root", children: [{ type: "paragraph", children: [
        url ? { type: "link", url, children: [{ type: "text", value: label }] } : { type: "text", value: label },
      ] }] }).trimEnd();
    }).join("\n");
  }
  return text;
}

/** Copy semantic HTML, without app styles or temporary preview URLs. */
export function replyFormatted(content: HTMLElement): { html: string; text: string } {
  const copy = content.cloneNode(true) as HTMLElement;
  for (const element of copy.querySelectorAll<HTMLElement>("*")) {
    for (const attribute of [...element.attributes]) {
      if (!["href", "src", "alt", "title", "colspan", "rowspan", "start", "type", "checked", "disabled"].includes(attribute.name)) {
        element.removeAttribute(attribute.name);
      }
    }
    for (const attribute of ["href", "src"]) {
      const value = element.getAttribute(attribute);
      if (value) {
        const url = portableUrl(value);
        if (url) element.setAttribute(attribute, url);
        else element.removeAttribute(attribute);
      }
    }
    if (element.tagName === "IMG" && !element.hasAttribute("src")) {
      element.replaceWith(document.createTextNode(element.getAttribute("alt") || "Image"));
    }
    if (element.tagName === "TH" || element.tagName === "TD") {
      element.style.cssText = "border: 1px solid #ccc; padding: 4px 8px;";
    }
    if (element.tagName === "TABLE") element.style.borderCollapse = "collapse";
  }
  function plain(node: Node): string {
    if (node.nodeType === Node.TEXT_NODE) {
      const text = node.textContent ?? "";
      // Markdown rendering inserts source newlines between block elements.
      if (/^\s*$/.test(text) && /^(DIV|UL|OL|TABLE|THEAD|TBODY|TR)$/.test(node.parentElement?.tagName ?? "")) return "";
      return text;
    }
    if (!(node instanceof HTMLElement)) return "";
    const text = [...node.childNodes].map(plain).join("");
    switch (node.tagName) {
      case "BR": return "\n";
      case "IMG": return node.getAttribute("alt") || "Image";
      case "LI": return `${node.parentElement?.tagName === "OL"
        ? Number(node.parentElement.getAttribute("start") || 1) + [...node.parentElement.children].indexOf(node) + "."
        : "•"} ${text.trim()}\n`;
      case "TD": case "TH": return text + "\t";
      case "TR": return text.trimEnd() + "\n";
      case "P": case "DIV": case "H1": case "H2": case "H3": case "H4": case "H5": case "H6":
      case "BLOCKQUOTE": case "PRE": case "UL": case "OL": case "TABLE": return text + "\n\n";
      default: return text;
    }
  }
  return { html: copy.innerHTML, text: plain(copy).trim() };
}
