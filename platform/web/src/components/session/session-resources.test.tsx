import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { expect, it } from "vitest";
import { sessionResources } from "@/lib/sessions/session-resources";
import { SessionResourceChips } from "./session-resources";

it("shows a Code mode resource chip only when code mode is enabled", () => {
  const render = (features: unknown) => renderToString(createElement(SessionResourceChips, {
    resources: sessionResources({ features }, null),
    slug: "demo",
  }));

  const enabled = render({ codeMode: {} });
  expect(enabled).toContain('aria-label="Code mode: Code mode"');

  const disabled = render({});
  expect(disabled).not.toContain('aria-label="Code mode: Code mode"');
});
