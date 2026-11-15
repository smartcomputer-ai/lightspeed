// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  MetaDetails,
  MetaDetailsAction,
  MetaDetailsFooter,
  MetaDetailsHeading,
  MetaDetailsList,
  MetaDetailsRow,
  RESOURCE_STATUS_TONE,
} from "@/components/meta-details";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

const onSelect = vi.fn();
const onLoadMore = vi.fn();

/// Opens a popover holding one of each kind of row and action.
async function open() {
  await act(async () => root.render(
    <MemoryRouter>
      <Popover>
        <PopoverTrigger>Details</PopoverTrigger>
        <PopoverContent>
          <MetaDetails>
            <MetaDetailsHeading title="Environment" description="Commands run here." />
            <MetaDetailsList label="Environments">
              <MetaDetailsRow primary="build-box" secondary={["Running", null, "", "edit access"]} tone="ready" badges={["Active", false]} />
              <MetaDetailsRow primary="Open build-box" href="/u/demo/environments/build-box" />
              <MetaDetailsRow primary="Restart" onSelect={onSelect} />
            </MetaDetailsList>
            <MetaDetailsFooter>
              <MetaDetailsAction onSelect={onLoadMore} keepOpen>Load more</MetaDetailsAction>
            </MetaDetailsFooter>
          </MetaDetails>
        </PopoverContent>
      </Popover>
    </MemoryRouter>,
  ));
  await act(async () => container.querySelector("button")!.click());
}

const list = () => document.body.querySelector('ul[aria-label="Environments"]');
const row = (text: string) => [...list()!.querySelectorAll("li > *")].find((element) => element.textContent?.includes(text)) as HTMLElement;

it("draws a plain row's dot, name, badges and facts, and keeps it out of the tab order", async () => {
  await open();
  const plain = row("build-box");
  expect(plain.tagName).toBe("DIV");
  expect(plain.getAttribute("tabindex")).toBeNull();
  expect(plain.querySelector("[data-tone]")!.getAttribute("data-tone")).toBe("ready");
  expect(plain.textContent).toBe("build-boxActiveRunning · edit access");
});

it("closes the popover when a linked row is followed", async () => {
  await open();
  const linked = row("Open build-box");
  expect(linked.tagName).toBe("A");
  expect(linked.getAttribute("href")).toBe("/u/demo/environments/build-box");
  await act(async () => linked.click());
  expect(list()).toBeNull();
});

it("runs an action row and closes the popover", async () => {
  await open();
  await act(async () => row("Restart").click());
  expect(onSelect).toHaveBeenCalledOnce();
  expect(list()).toBeNull();
});

it("keeps the popover open for a keep-open action", async () => {
  await open();
  const more = [...document.body.querySelectorAll("button")].find((button) => button.textContent === "Load more")!;
  await act(async () => more.click());
  expect(onLoadMore).toHaveBeenCalledOnce();
  expect(list()).not.toBeNull();
});

it("maps every resource tone onto a status dot tone", () => {
  expect(RESOURCE_STATUS_TONE).toEqual({ ok: "ready", idle: "idle", busy: "waiting", problem: "attention" });
});
