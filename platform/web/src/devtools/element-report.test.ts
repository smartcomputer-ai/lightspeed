// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cssPath, describeElement, formatReport, sourceFileFromStack, uniqueSelector } from "./element-report";

afterEach(() => {
  document.body.innerHTML = "";
});

function mount(html: string) {
  document.body.innerHTML = html;
}

describe("element report", () => {
  it("builds a full path from <html> that resolves to the element", () => {
    mount(`<div id="root"><main><section><button data-slot="button">One</button><button data-slot="button">Two</button></section></main></div>`);
    const second = document.querySelectorAll("button")[1]!;
    const path = cssPath(second);
    expect(path).toBe('html > body > div#root > main > section > button[data-slot="button"]:nth-child(2)');
    expect(document.querySelectorAll(path)).toHaveLength(1);
    expect(document.querySelector(path)).toBe(second);
  });

  it("shortens the unique selector to the first unambiguous suffix", () => {
    mount(`<div id="root"><ul><li><a>Open</a></li><li><a>Open</a></li></ul><nav><a>Open</a></nav></div>`);
    const link = document.querySelector("nav a")!;
    const selector = uniqueSelector(link);
    expect(document.querySelectorAll(selector)).toHaveLength(1);
    expect(document.querySelector(selector)).toBe(link);
    expect(selector.startsWith("html")).toBe(false);
  });

  it("stops at an id and escapes it", () => {
    mount(`<div id="panel:1"><span>Inside</span></div>`);
    const span = document.querySelector("span")!;
    const selector = uniqueSelector(span);
    expect(document.querySelector(selector)).toBe(span);
  });

  it("reports text, role, accessible name and value, hiding passwords", () => {
    mount(`<label for="name">Name</label><input id="name" value="Planner" />
      <input id="secret" type="password" value="hunter2" aria-label="Secret" />
      <button aria-label="Close dialog">×</button>`);
    const name = describeElement(document.getElementById("name")!);
    expect(name).toMatchObject({ role: "textbox", accessibleName: "Name", value: "Planner" });
    const secret = describeElement(document.getElementById("secret")!);
    expect(secret.value).toBe("(hidden password)");
    expect(secret.element).not.toContain("hunter2");
    const button = describeElement(document.querySelector("button")!);
    expect(button).toMatchObject({ role: "button", accessibleName: "Close dialog" });
  });

  it("formats labelled lines with the note first and skips empty fields", () => {
    mount(`<p>Hello <b>world</b></p>`);
    const text = formatReport(describeElement(document.querySelector("p")!), "  Too much padding  ");
    const lines = text.split("\n");
    expect(lines[0]).toBe("Feedback: Too much padding");
    expect(lines[1]).toMatch(/^Page: /);
    expect(text).toContain("Selector: html > body > p");
    expect(text).toContain("Element: <p>");
    expect(text).not.toContain("Value:");
    expect(formatReport(describeElement(document.querySelector("p")!))).not.toContain("Feedback:");
  });

  it("finds the first application file in Chrome and Firefox stacks", () => {
    const chrome = [
      "Error: react-stack-top-frame",
      "    at exports.jsxDEV (http://localhost:5173/app/node_modules/.vite/deps/react_jsx-dev-runtime.js?v=1:250:30)",
      "    at SessionList (http://localhost:5173/app/src/pages/SessionsPage.tsx?t=17:412:9)",
    ].join("\n");
    expect(sourceFileFromStack(chrome)).toBe("src/pages/SessionsPage.tsx");
    const firefox = "jsxDEV@http://localhost:5173/app/node_modules/.vite/deps/react.js:1:2\nSessionList@http://localhost:5173/app/src/pages/SessionsPage.tsx:412:9";
    expect(sourceFileFromStack(firefox)).toBe("src/pages/SessionsPage.tsx");
    expect(sourceFileFromStack(undefined)).toBeUndefined();
  });
});
