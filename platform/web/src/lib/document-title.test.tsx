// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  DocumentTitleProvider,
  TITLE_APP_NAME,
  TITLE_APP_SEPARATOR,
  TITLE_SEGMENT_MAX_LENGTH,
  TITLE_SEPARATOR,
  documentTitle,
  useDocumentTitle,
  type TitleSegment,
} from "./document-title";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  document.title = "";
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

/// The path's segments, then the app.
const join = (...segments: string[]) => `${segments.join(TITLE_SEPARATOR)}${TITLE_APP_SEPARATOR}${TITLE_APP_NAME}`;

function Page({ trail }: { trail: TitleSegment[] }) {
  useDocumentTitle(trail);
  return null;
}

it("reads the path backwards and ends with the app", () => {
  expect(documentTitle(["Explain the auth middleware", "Sessions", "Software Factory"])).toBe(
    "Explain the auth middleware / Sessions / Software Factory – Lightspeed",
  );
  expect(documentTitle([])).toBe(TITLE_APP_NAME);
});

it("drops empty segments and folds whitespace", () => {
  expect(documentTitle([undefined, "", "  ", null, false, " Two\n lines ", "Sessions"])).toBe(
    join("Two lines", "Sessions"),
  );
});

it("shortens a long segment to the limit with an ellipsis", () => {
  const [first] = documentTitle(["x".repeat(TITLE_SEGMENT_MAX_LENGTH + 10)]).split(TITLE_APP_SEPARATOR);
  expect(first).toHaveLength(TITLE_SEGMENT_MAX_LENGTH);
  expect(first!.endsWith("…")).toBe(true);
});

it("puts the open page's segments before the provider's base", async () => {
  await act(async () => root.render(
    <DocumentTitleProvider base={["Sessions", "Software Factory"]}>
      <Page trail={["Explain the auth middleware"]} />
    </DocumentTitleProvider>,
  ));
  expect(document.title).toBe(join("Explain the auth middleware", "Sessions", "Software Factory"));
});

it("falls back to the base when the page leaves", async () => {
  const render = (open: boolean) => root.render(
    <DocumentTitleProvider base={["Sessions", "Software Factory"]}>
      {open && <Page trail={["A session"]} />}
    </DocumentTitleProvider>,
  );
  await act(async () => render(true));
  await act(async () => render(false));
  expect(document.title).toBe(join("Sessions", "Software Factory"));
});

it("takes the next page's title when one replaces another", async () => {
  const render = (id: string) => root.render(
    <DocumentTitleProvider base={["Sessions"]}>
      <Page key={id} trail={[id]} />
    </DocumentTitleProvider>,
  );
  await act(async () => render("First"));
  await act(async () => render("Second"));
  expect(document.title).toBe(join("Second", "Sessions"));
});

it("leaves the surrounding page's title to an embedded pane with no trail", async () => {
  const render = (id: string) => root.render(
    <DocumentTitleProvider base={["Bots"]}>
      <Page trail={["Main", "Triage bot"]} />
      <Page key={id} trail={[]} />
    </DocumentTitleProvider>,
  );
  await act(async () => render("one"));
  await act(async () => render("two"));
  expect(document.title).toBe(join("Main", "Triage bot", "Bots"));
});

it("is the whole title outside a provider", async () => {
  await act(async () => root.render(<Page trail={["Sign in"]} />));
  expect(document.title).toBe(join("Sign in"));
});
