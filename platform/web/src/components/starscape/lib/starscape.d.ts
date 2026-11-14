/** Types for starscape.js, the renderer copied from the Starscape Creator. */

export interface StarscapeConfig {
  seed: number;
  centerX: number;
  centerY: number;
  depth: number;
  starCount: number;
  starSize: number;
  starBrightness: number;
  starSizeVar: number;
  starColorVar: number;
  starClustering: number;
  starSpikes: number;
  starSpikeSize: number;
  starTwinkle: number;
  streakCount: number;
  streakSpeed: number;
  streakSpread: number;
  streakThickness: number;
  streakBrightness: number;
  streakSoftness: number;
  streakTailFade: number;
  streakHeadGlow: number;
  streakEnergy: number;
  streakColorVar: number;
  streakChroma: number;
  streakMaxLength: number;
  nebulaAmount: number;
  nebulaScale: number;
  nebulaHue: number;
  airglow: number;
  bloomAmount: number;
  bloomRadius: number;
  vignette: number;
  grain: number;
  exposure: number;
  flowSpeed: number;
  twinkleSpeed: number;
  fps: number;
}

/** A window onto a larger frame, all in canvas pixels. */
export interface StarscapeViewport {
  frameWidth: number;
  frameHeight: number;
  /** Top-left corner of the window within the frame. */
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Fully visible within `start` px of the vanishing point, gone beyond `end`. */
export interface StarscapeFade {
  start: number;
  end: number;
  /** Also clears the centre: gone within `start` px, fully visible again by `end`. */
  inner?: { start: number; end: number };
}

export interface StarscapeOptions {
  /** A transparent canvas, which the radial fade needs. */
  alpha?: boolean;
  /** Draw the stars, streaks and nebula only: no black sky, airglow, vignette or grain. Implies `alpha`. */
  transparentBackground?: boolean;
}

export class Starscape {
  constructor(canvas: HTMLCanvasElement, overrides?: Partial<StarscapeConfig>, options?: StarscapeOptions);
  readonly config: StarscapeConfig;
  readonly canvas: HTMLCanvasElement;
  readonly frame: number;
  resize(width: number, height: number): this;
  setViewport(viewport: StarscapeViewport): this;
  setFade(fade: StarscapeFade | null): this;
  update(patch: Partial<StarscapeConfig>): this;
  reset(overrides?: Partial<StarscapeConfig>): this;
  advance(frames?: number): this;
  rewind(): this;
  render(): this;
}
