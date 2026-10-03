// @vitest-environment jsdom
import { expect, it } from "vitest";
import { replyFormatted, replyMarkdown } from "./reply-copy";

function content(html: string) {
  const element = document.createElement("div");
  element.innerHTML = html;
  return element;
}

it("preserves Markdown source including code, whitespace, tables, and ordinary links", () => {
  const text = '## Heading\n\n**Bold** and `file:example`.\n\n```md\n[x](media:example)\n```\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n[Site](https://example.com)\n';
  expect(replyMarkdown(text, content(""))).toBe(text);
});

it("copies inline and reference-style attachments as stable viewer links, leaving code intact", () => {
  const text = '[**Report**](file:report)\n\n![Photo](media:photo)\n\n[Report again][ref]\n\n[ref]: file:report "Report"\n\n`[Report](file:report)`';
  const body = content('<a data-attachment-reference="file:report" href="/blobs/report?workspace=w&amp;path=a.md">Report</a><a data-attachment-reference="media:photo" href="/blobs/photo"><img src="blob:preview" alt="Photo"></a>');
  const copied = replyMarkdown(text, body);
  const report = new URL("/blobs/report?workspace=w&path=a.md", window.location.href).href;
  expect(copied).toContain(`[**Report**](${report})`);
  expect(copied).toContain(`[Photo](${new URL("/blobs/photo", window.location.href).href})`);
  expect(copied).toContain(`[ref]: ${report} "Report"`);
  expect(copied).toContain("`[Report](file:report)`");
  expect(copied).not.toContain("blob:preview");
});

it("reports unresolved attachment links instead of copying unusable handles", () => {
  expect(() => replyMarkdown("[Report](file:report)", content("Report"))).toThrow("An attachment link is unavailable");
});

it("copies reference-style attachment images as links to the viewer", () => {
  const copied = replyMarkdown("![Photo][image]\n\n[image]: media:photo", content('<a data-attachment-reference="media:photo" href="/blobs/photo">Photo</a>'));
  expect(copied).toContain(`[Photo](${new URL("/blobs/photo", window.location.href).href})`);
  expect(copied).not.toContain("![Photo]");
});

it("appends escaped citations as Markdown links", () => {
  expect(replyMarkdown("Answer.", content("Answer."), [{ url: "https://example.com/a", title: "A [source]" }]))
    .toBe("Answer.\n\nSources\n\n- [A \\[source\\]](https://example.com/a)");
});

it("copies semantic formatting and readable plain text without application styles", () => {
  const { html, text } = replyFormatted(content('<div class="markdown"><h2>Heading</h2><p><strong>Bold</strong> and <em>italic</em>.</p><ol start="3"><li>First</li><li>Second</li></ol><pre class="code"><code>const a = 1;\n  a++;</code></pre><table><tbody><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></tbody></table><p><a class="link" href="/source">Source</a></p></div>'));
  expect(html).toContain("<strong>Bold</strong>");
  expect(html).toContain("<em>italic</em>");
  expect(html).toContain("<code>const a = 1;\n  a++;</code>");
  expect(html).toContain(`href="${new URL("/source", window.location.href).href}"`);
  expect(html).not.toContain("class=");
  expect(text).toContain("Heading\n\nBold and italic.\n\n3. First\n4. Second");
  expect(text).toContain("const a = 1;\n  a++;");
  expect(text).toContain("A\tB\n1\t2");
});

it("turns temporary image previews into named viewer links in formatted copy", () => {
  const { html, text } = replyFormatted(content('<p><a data-attachment-reference="media:photo" href="/blobs/photo"><img src="blob:preview" alt="Photo"></a></p><img src="https://example.com/image.png" alt="External">'));
  expect(html).toContain(`<a href="${new URL("/blobs/photo", window.location.href).href}">Photo</a>`);
  expect(html).toContain('src="https://example.com/image.png"');
  expect(html).not.toContain("blob:");
  expect(html).not.toContain("data-attachment-reference");
  expect(text).toBe("Photo\n\nExternal");
});
