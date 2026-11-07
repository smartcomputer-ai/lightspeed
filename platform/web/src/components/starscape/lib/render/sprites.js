/**
 * Pre-rendered additive sprites.
 *
 * Drawing tens of thousands of stars with per-star gradients is far too slow for
 * a live-editing tool. Instead every light source is baked once into a small
 * offscreen canvas and then blitted with `drawImage`, which the browser can
 * hardware-accelerate.
 *
 * All sprites are authored for `globalCompositeOperation = 'lighter'` (additive
 * light). Each pixel stores its light as premultiplied colour with the alpha of
 * its brightest channel (see `putLight`): canvases keep pixels premultiplied,
 * so additive blending still adds exactly `light * globalAlpha` at the same
 * 8-bit precision, while the dark surround of each sprite stays transparent and
 * the sprites also work on a canvas without a black sky.
 */

import { clamp, smoothstep } from '../core/noise.js';

/**
 * Writes premultiplied light (0-255 per channel) into an ImageData pixel,
 * which is unpremultiplied: alpha is the brightest channel and the colour is
 * the light divided by it.
 */
function putLight(data, i, r, g, b) {
  r = Math.min(255, r);
  g = Math.min(255, g);
  b = Math.min(255, b);
  const peak = Math.max(r, g, b);
  if (peak <= 0) {
    data[i] = data[i + 1] = data[i + 2] = data[i + 3] = 0;
    return;
  }
  const scale = 255 / peak;
  data[i] = r * scale;
  data[i + 1] = g * scale;
  data[i + 2] = b * scale;
  data[i + 3] = peak;
}

/** Number of discrete colour buckets shared by stars and streaks. */
export const COLOR_BUCKETS = 12;

const STAR_SPRITE_SIZE = 128;
const SPIKE_SPRITE_SIZE = 256;

/**
 * Point-spread function of a star as recorded by a real lens: a tight Gaussian
 * core riding on a much wider exponential halo. The halo is what makes bright
 * stars feel like they have presence instead of looking like drawn dots.
 */
const CORE_SIGMA = 0.062;
const HALO_AMPLITUDE = 0.17;
const HALO_SIGMA = 0.16;

/**
 * Ratio between the sprite's drawn width and the star's nominal size in pixels.
 * Derived from CORE_SIGMA so that a size of 1 yields a ~1px full-width-half-max
 * core with its halo extending beyond it.
 */
export const STAR_SPRITE_SCALE = 1 / (2.3548 * CORE_SIGMA);

/** Ratio between a streak sprite's drawn height and its nominal thickness. */
export const STREAK_HEIGHT_SCALE = 5.4;

function createCanvas(width, height) {
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  return canvas;
}

function starIntensity(rn) {
  if (rn >= 1) return 0;
  const core = Math.exp(-(rn * rn) / (2 * CORE_SIGMA * CORE_SIGMA));
  const halo = HALO_AMPLITUDE * Math.exp(-rn / HALO_SIGMA);
  // Window the tail to zero at the sprite edge so sprites never show a seam.
  return (core + halo) * smoothstep(1, 0.82, rn);
}

/** Smallest star mip level; below this the sprite carries no useful detail. */
const MIN_STAR_MIP = 8;

/**
 * Builds a mip chain for a star colour, largest first.
 *
 * Most stars in a dense field are a couple of pixels across. Sampling a 128px
 * master down to 3px for every one of them is both slow and soft, so each colour
 * is pre-reduced by successive halving and the draw picks the smallest level
 * that still covers the target size. This is the single largest win in the
 * field-drawing loop.
 */
function buildStarMips(rgb) {
  const mips = [buildStarSprite(rgb)];
  let source = mips[0];
  while (source.width > MIN_STAR_MIP * 2) {
    const half = createCanvas(source.width >> 1, source.height >> 1);
    const ctx = half.getContext('2d');
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = 'high';
    ctx.drawImage(source, 0, 0, half.width, half.height);
    mips.push(half);
    source = half;
  }
  return mips;
}

/**
 * Chooses the mip level to draw at `targetSize` pixels: the smallest level that
 * is still at least as large as the target, so the sampler never has to reduce
 * by more than a factor of two.
 */
export function pickMip(mips, targetSize) {
  let level = 0;
  while (level + 1 < mips.length && mips[level + 1].width >= targetSize) level++;
  return mips[level];
}

/** Builds the base star point-spread sprite for one colour. */
function buildStarSprite(rgb) {
  const size = STAR_SPRITE_SIZE;
  const canvas = createCanvas(size, size);
  const ctx = canvas.getContext('2d');
  const image = ctx.createImageData(size, size);
  const data = image.data;
  const center = (size - 1) / 2;
  const inv = 1 / center;

  for (let y = 0; y < size; y++) {
    const dy = (y - center) * inv;
    for (let x = 0; x < size; x++) {
      const dx = (x - center) * inv;
      const intensity = starIntensity(Math.sqrt(dx * dx + dy * dy));
      const i = (y * size + x) * 4;
      putLight(data, i, rgb[0] * intensity, rgb[1] * intensity, rgb[2] * intensity);
    }
  }
  ctx.putImageData(image, 0, 0);
  return canvas;
}

/**
 * Four-armed diffraction spikes, the signature of a lens with a straight-bladed
 * aperture. Applied only to the brightest stars, where a real photograph would
 * actually show them.
 */
function buildSpikeSprite(rgb) {
  const size = SPIKE_SPRITE_SIZE;
  const canvas = createCanvas(size, size);
  const ctx = canvas.getContext('2d');
  const image = ctx.createImageData(size, size);
  const data = image.data;
  const center = (size - 1) / 2;
  const inv = 1 / center;

  const arm = (along, across) =>
    Math.exp(-Math.abs(across) / 0.0075) * Math.exp(-Math.abs(along) / 0.26);

  const diag = 0.70710678;
  for (let y = 0; y < size; y++) {
    const dy = (y - center) * inv;
    for (let x = 0; x < size; x++) {
      const dx = (x - center) * inv;
      const r = Math.sqrt(dx * dx + dy * dy);
      // Cardinal arms are strong; the diagonal pair is a faint secondary order.
      const u = (dx + dy) * diag;
      const v = (dx - dy) * diag;
      let intensity = arm(dx, dy) + arm(dy, dx) + 0.22 * (arm(u, v) + arm(v, u));
      intensity *= smoothstep(1, 0.55, r);
      const i = (y * size + x) * 4;
      putLight(
        data,
        i,
        clamp(rgb[0] * intensity, 0, 255),
        clamp(rgb[1] * intensity, 0, 255),
        clamp(rgb[2] * intensity, 0, 255),
      );
    }
  }
  ctx.putImageData(image, 0, 0);
  return canvas;
}

/**
 * The body of a warp streak, drawn horizontally with its tail at x = 0 and its
 * head at x = width.
 *
 * Two profiles are baked in:
 *  - across the streak, the same core-plus-halo point spread as a star, so a
 *    trail reads as a smeared star rather than as a stroked line;
 *  - along the streak, a ramp from the dim tail up to the bright head, which is
 *    how a trail looks when the source is approaching and accelerating.
 *
 * @param {[number,number,number]} rgb
 * @param {number} softness   0..1, widens the halo relative to the core
 * @param {number} tailFade   0..1, how far the tail drops below the head
 * @param {number} height     sprite height in pixels, chosen by the caller to
 *                            keep the on-screen downscale factor modest
 */
function buildStreakSprite(rgb, softness, tailFade, height) {
  const width = 384;
  const canvas = createCanvas(width, height);
  const ctx = canvas.getContext('2d');
  const image = ctx.createImageData(width, height);
  const data = image.data;
  const center = (height - 1) / 2;
  const inv = 1 / center;

  const coreSigma = 0.1 + softness * 0.08;
  const haloSigma = 0.18 + softness * 0.5;
  const haloAmp = 0.1 + softness * 0.35;
  const tailLevel = 1 - tailFade;
  // A sharper ramp for a harder fade keeps short streaks from looking blunt.
  const rampPower = 1 + tailFade * 2.2;

  const crossSection = new Float32Array(height);
  for (let y = 0; y < height; y++) {
    const dy = (y - center) * inv;
    const core = Math.exp(-(dy * dy) / (2 * coreSigma * coreSigma));
    const halo = haloAmp * Math.exp(-Math.abs(dy) / haloSigma);
    crossSection[y] = (core + halo) * smoothstep(1, 0.7, Math.abs(dy));
  }

  for (let x = 0; x < width; x++) {
    const t = x / (width - 1);
    // Round off the extreme tail so it dissolves instead of ending in a chop.
    const along =
      (tailLevel + (1 - tailLevel) * Math.pow(t, rampPower)) *
      smoothstep(0, 0.035, t);
    for (let y = 0; y < height; y++) {
      const intensity = along * crossSection[y];
      const i = (y * width + x) * 4;
      putLight(data, i, rgb[0] * intensity, rgb[1] * intensity, rgb[2] * intensity);
    }
  }
  ctx.putImageData(image, 0, 0);
  return canvas;
}

/** Sprite height that keeps the on-screen downscale factor around 6x. */
export function streakSpriteHeight(thicknessPx) {
  return clamp(Math.round(thicknessPx * 6), 12, 96);
}

/** A tile of monochrome noise, used for film grain. */
export function buildGrainTile(size = 512, random = Math.random) {
  const canvas = createCanvas(size, size);
  const ctx = canvas.getContext('2d');
  const image = ctx.createImageData(size, size);
  const data = image.data;
  for (let i = 0; i < data.length; i += 4) {
    // Squaring biases toward dark, giving sparse specks rather than flat fuzz.
    const v = Math.pow(random(), 2) * 255;
    data[i] = v;
    data[i + 1] = v;
    data[i + 2] = v;
    data[i + 3] = 255;
  }
  ctx.putImageData(image, 0, 0);
  return canvas;
}

/**
 * Lazily builds and caches every sprite variant.
 *
 * Cache keys embed the parameters a sprite was baked with, so changing e.g.
 * softness transparently invalidates only the affected sprites.
 */
export class SpriteCache {
  constructor() {
    this.cache = new Map();
  }

  get(key, build) {
    let value = this.cache.get(key);
    if (value === undefined) {
      value = build();
      this.cache.set(key, value);
    }
    return value;
  }

  /**
   * Star point-spread mip chain, largest first. Pass the result through
   * `pickMip` with the intended draw size.
   *
   * @param {string} id  identity of the colour (palette key plus bucket index)
   */
  star(id, rgb) {
    return this.get('star:' + id, () => buildStarMips(rgb));
  }

  /** Diffraction-spike sprite for a colour. */
  spike(id, rgb) {
    return this.get('spike:' + id, () => buildSpikeSprite(rgb));
  }

  /** Warp-streak body sprite for a colour and brightness profile. */
  streak(id, rgb, softness, tailFade, height) {
    const s = Math.round(softness * 20) / 20;
    const f = Math.round(tailFade * 20) / 20;
    const key = 'streak:' + id + ':' + s + ':' + f + ':' + height;
    return this.get(key, () => buildStreakSprite(rgb, s, f, height));
  }

  clear() {
    this.cache.clear();
  }
}
