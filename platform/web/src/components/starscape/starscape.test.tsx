// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it } from "vitest";
import { SITE_FRAME, Starscape, starscapeWindow } from "./starscape";

const centred = { centerX: 0.5, centerY: 0.5 };

describe("starscapeWindow", () => {
  it("draws the whole host without a fade, with the frame's centre on the vanishing point", () => {
    const area = starscapeWindow({ width: 200, height: 100, cx: 100, cy: 50, scale: 1, ratio: 1, config: centred });
    expect(area).toMatchObject({ left: 0, top: 0, width: 200, height: 100 });
    expect(area?.viewport).toEqual({
      frameWidth: SITE_FRAME.width,
      frameHeight: SITE_FRAME.height,
      x: SITE_FRAME.width / 2 - 100,
      y: SITE_FRAME.height / 2 - 50,
      width: 200,
      height: 100,
    });
  });

  it("draws only the part within the fade's outer radius, in device pixels at 2x", () => {
    const area = starscapeWindow({
      width: 256,
      height: 64,
      cx: 32,
      cy: 32,
      fade: { start: 16, end: 32 },
      scale: 1,
      ratio: 2,
      config: centred,
    });
    expect(area).toMatchObject({ left: 0, top: 0, width: 64, height: 64 });
    expect(area?.viewport).toMatchObject({
      frameWidth: SITE_FRAME.width * 2,
      x: SITE_FRAME.width - 64,
      width: 128,
      height: 128,
    });
  });

  it("scales the frame and draws nothing when the fade lies outside the host", () => {
    expect(starscapeWindow({ width: 100, height: 100, cx: 50, cy: 50, scale: 0.5, ratio: 1, config: centred })?.viewport.frameWidth)
      .toBe(SITE_FRAME.width * 0.5);
    expect(starscapeWindow({ width: 100, height: 100, cx: -80, cy: 50, fade: { start: 0, end: 40 }, scale: 1, ratio: 1, config: centred }))
      .toBeNull();
  });
});

it("renders an inert layer where there is no 2D canvas", async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  await act(async () => root.render(<Starscape className="absolute inset-0" fade={{ start: 8, end: 24 }} />));
  const host = container.querySelector("[data-starscape]")!;
  expect(host.getAttribute("aria-hidden")).toBe("true");
  expect(host.querySelector("canvas")).not.toBeNull();
  await act(async () => root.unmount());
});
