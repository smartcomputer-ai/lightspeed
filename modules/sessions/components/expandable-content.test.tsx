import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ExpandableContent } from "./expandable-content";

describe("ExpandableContent", () => {
  it("renders the bounded preview, marker, and CAS expansion action", () => {
    const html = renderToString(createElement(ExpandableContent, {
      text: "bounded preview",
      truncated: true,
      blobRef: "sha256:full",
      loadFullText: async () => "complete body",
      children: (text: string) => createElement("pre", null, text),
    }));

    expect(html).toContain("bounded preview");
    expect(html).toContain("Truncated preview.");
    expect(html).toContain("Expand full entry");
  });
});
