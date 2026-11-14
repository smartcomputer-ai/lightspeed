// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { describeElement, formatReport, slotPath, sourceFileFromStack } from "./element-report";

afterEach(() => {
  document.body.innerHTML = "";
});

function mount(html: string) {
  document.body.innerHTML = html;
}

describe("element report", () => {
  it("lists the data-slot names around the element, outermost first", () => {
    mount(`<div data-slot="card"><div><div data-slot="field"><div data-slot="field"><span>Name</span></div></div></div></div>
      <p>Plain</p>`);
    expect(slotPath(document.querySelector("span")!)).toEqual(["card", "field"]);
    expect(slotPath(document.querySelector("p")!)).toEqual([]);
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

  it("formats labelled lines and skips empty fields", () => {
    mount(`<div data-slot="card"><p data-slot="card-description">Hello <b>world</b></p></div>`);
    const text = formatReport(describeElement(document.querySelector("p")!));
    expect(text).toMatch(/^Page: /);
    expect(text).toContain("Slots: card › card-description");
    expect(text).not.toContain("Selector:");
    expect(text).toContain('Element: <p data-slot="card-description">');
    expect(text).not.toContain("Value:");
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
