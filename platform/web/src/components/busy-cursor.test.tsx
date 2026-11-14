// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider, useMutation } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it } from "vitest";
import { BusyCursor } from "./busy-cursor";

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let mutate: ((blocking: boolean) => void) | undefined;
let release: (() => void) | undefined;

function Saves() {
  const plain = useMutation({ mutationFn: () => new Promise<void>((resolve) => { release = resolve; }) });
  const blocking = useMutation({ meta: { blocking: true }, mutationFn: () => new Promise<void>((resolve) => { release = resolve; }) });
  mutate = (isBlocking) => (isBlocking ? blocking : plain).mutate();
  return null;
}

beforeEach(async () => {
  client = new QueryClient();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => root.render(<QueryClientProvider client={client}><BusyCursor /><Saves /></QueryClientProvider>));
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

const cursor = () => document.documentElement.dataset.cursor;
/// Mutation state reaches its observers on a short timer.
const step = (run: () => void) => act(async () => { run(); await new Promise((resolve) => setTimeout(resolve, 10)); });

it("shows progress while a change is saved, and clears it after", async () => {
  expect(cursor()).toBeUndefined();
  await step(() => mutate!(false));
  expect(cursor()).toBe("progress");
  await step(() => release!());
  expect(cursor()).toBeUndefined();
});

it("makes the whole page wait while a blocking change runs", async () => {
  await step(() => mutate!(true));
  expect(cursor()).toBe("wait");
  await step(() => release!());
  expect(cursor()).toBeUndefined();
});
