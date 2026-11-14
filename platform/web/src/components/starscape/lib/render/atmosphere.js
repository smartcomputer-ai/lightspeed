/**
 * Everything that is not a star: the faint structure of the sky itself and the
 * imperfections of the camera that recorded it.
 *
 * None of these layers are individually noticeable, which is the point. A pure
 * black background with perfect points on it looks synthetic; real astrophotos
 * carry nebular haze, a lifted sky glow, lens bloom, falloff toward the corners
 * and sensor grain. Adding them at low intensity is what pushes the image from
 * "drawn" to "photographed".
 *
 * The two layers that never change between frames — the sky background and the
 * vignette — are baked into full-size canvases and blitted, rather than being
 * re-evaluated per pixel every frame. Only bloom and grain, which genuinely
 * depend on the frame, are computed live.
 */

import { fbm, clamp, smoothstep } from '../core/noise.js';
import { hslToRgb } from '../core/color.js';
import { buildGrainTile } from './sprites.js';
import { mulberry32 } from '../core/rng.js';

/** Long edge of the generated nebula texture, before upscaling. */
const NEBULA_TEXTURE_SIZE = 320;

/** Edge length of the tiling grain texture. */
const GRAIN_TILE_SIZE = 512;

/** Fraction of full resolution the bloom pass works at. */
const BLOOM_SCALE = 0.25;

/** A full-size canvas rebuilt only when its inputs change. */
class CachedLayer {
  constructor() {
    this.canvas = null;
    this.key = '';
  }

  /**
   * Returns the cached canvas, repainting it through `paint` if `key` changed.
   *
   * @param {string} key
   * @param {number} width
   * @param {number} height
   * @param {(ctx: CanvasRenderingContext2D) => void} paint
   */
  get(key, width, height, paint) {
    const stale =
      this.key !== key ||
      !this.canvas ||
      this.canvas.width !== width ||
      this.canvas.height !== height;

    if (stale) {
      if (!this.canvas) this.canvas = document.createElement('canvas');
      this.canvas.width = width;
      this.canvas.height = height;
      const ctx = this.canvas.getContext('2d');
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, width, height);
      paint(ctx);
      this.key = key;
    }
    return this.canvas;
  }

  invalidate() {
    this.key = '';
  }
}

export class Atmosphere {
  constructor() {
    this.background = new CachedLayer();
    this.vignetteLayer = new CachedLayer();
    this.nebulaTexture = null;
    this.nebulaTextureKey = '';
    this.grainTile = null;
  }

  /**
   * Paints the sky itself: black, plus a faint airglow lift around the vanishing
   * point and the nebular haze. Opaque, so it also serves as the frame clear.
   * Transparent, it is the nebula alone over a cleared frame.
   */
  drawBackground(ctx, view, config, transparent = false) {
    const key = [
      transparent,
      config.seed | 0,
      config.airglow,
      config.exposure,
      config.centerX,
      config.centerY,
      config.nebulaAmount,
      config.nebulaScale,
      config.nebulaHue,
      view.width,
      view.height,
      view.viewX,
      view.viewY,
    ].join('|');

    // Cached at the size of the window the canvas shows, painted in frame
    // coordinates; anything outside the window is clipped away.
    const layer = this.background.get(key, view.viewWidth, view.viewHeight, (lctx) => {
      lctx.translate(-view.viewX, -view.viewY);
      if (!transparent) {
        lctx.fillStyle = '#000000';
        lctx.fillRect(view.viewX, view.viewY, view.viewWidth, view.viewHeight);
        this.paintAirglow(lctx, view, config);
      }
      this.paintNebula(lctx, view, config, transparent);
    });

    ctx.globalCompositeOperation = 'source-over';
    ctx.globalAlpha = 1;
    if (transparent) ctx.clearRect(0, 0, view.viewWidth, view.viewHeight);
    ctx.drawImage(layer, 0, 0);
  }

  /**
   * Faint sky glow: a broad, very dim radial lift centred on the vanishing
   * point. Real skies are never absolutely black, and the gradient gives the
   * frame depth for free.
   */
  paintAirglow(ctx, view, config) {
    const amount = config.airglow * config.exposure;
    if (amount <= 0.001) return;

    const radius = view.diag * 0.75;
    const gradient = ctx.createRadialGradient(view.cx, view.cy, 0, view.cx, view.cy, radius);
    const peak = amount * 0.09;
    gradient.addColorStop(0, 'rgba(104,132,190,' + peak.toFixed(4) + ')');
    gradient.addColorStop(0.45, 'rgba(72,92,142,' + (peak * 0.42).toFixed(4) + ')');
    gradient.addColorStop(1, 'rgba(20,26,48,0)');

    ctx.globalCompositeOperation = 'lighter';
    ctx.fillStyle = gradient;
    ctx.fillRect(view.viewX, view.viewY, view.viewWidth, view.viewHeight);
    ctx.globalCompositeOperation = 'source-over';
  }

  /**
   * Nebular haze: soft fractal clouds. Generated small and upscaled, which is
   * both fast and appropriate — the structure is genuinely low frequency.
   */
  paintNebula(ctx, view, config, transparent = false) {
    if (config.nebulaAmount <= 0.001) return;

    // The haze covers the frame and the window together: a window reaching
    // past the frame (a wide host around a tall frame) would otherwise show
    // the clouds stopping at the frame's edge. Generated at that shape, so
    // it is not stretched; a window inside the frame leaves it the frame.
    const left = Math.min(0, view.viewX);
    const top = Math.min(0, view.viewY);
    const width = Math.max(view.width, view.viewX + view.viewWidth) - left;
    const height = Math.max(view.height, view.viewY + view.viewHeight) - top;
    const aspect = width / height;

    const key = [
      transparent,
      config.seed | 0,
      Math.round(config.nebulaScale * 100),
      Math.round(config.nebulaHue),
      Math.round(aspect * 100),
    ].join(':');

    if (this.nebulaTextureKey !== key) {
      this.nebulaTexture = buildNebulaTexture(config, aspect, transparent);
      this.nebulaTextureKey = key;
    }

    ctx.globalCompositeOperation = 'lighter';
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = 'high';
    // Scaled well down: the haze should register as depth, never as scenery.
    ctx.globalAlpha = clamp(config.nebulaAmount * config.exposure * 0.42, 0, 1);
    ctx.drawImage(this.nebulaTexture, left, top, width, height);
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = 'source-over';
  }

  /**
   * Lens bloom: bright sources bleeding into their surroundings. Implemented by
   * blurring a downscaled copy of the scene and adding it back, which is the
   * cheap approximation every real-time renderer uses.
   *
   * Resampling quality is deliberately left low on both the reduction and the
   * expansion — the intermediate is a heavy blur, so a better filter would cost
   * real time to produce a result nobody can distinguish.
   */
  drawBloom(ctx, view, config, scratch) {
    const amount = config.bloomAmount;
    if (amount <= 0.001 || config.bloomRadius <= 0) return;

    const w = Math.max(2, Math.round(view.viewWidth * BLOOM_SCALE));
    const h = Math.max(2, Math.round(view.viewHeight * BLOOM_SCALE));
    const small = scratch.acquire(w, h);
    const sctx = small.getContext('2d');

    sctx.setTransform(1, 0, 0, 1, 0, 0);
    sctx.globalAlpha = 1;
    sctx.globalCompositeOperation = 'copy';
    sctx.imageSmoothingEnabled = true;
    sctx.imageSmoothingQuality = 'low';
    sctx.filter = 'blur(' + (config.bloomRadius * view.unit * BLOOM_SCALE).toFixed(2) + 'px)';
    sctx.drawImage(ctx.canvas, 0, 0, w, h);
    sctx.filter = 'none';
    sctx.globalCompositeOperation = 'source-over';

    ctx.save();
    ctx.globalCompositeOperation = 'lighter';
    ctx.globalAlpha = clamp(amount, 0, 1);
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = 'low';
    ctx.drawImage(small, 0, 0, view.viewWidth, view.viewHeight);
    ctx.restore();
  }

  /**
   * Sensor grain. A single noise tile is repeated with a per-frame offset, so
   * animation gets moving grain without regenerating megabytes of noise.
   */
  drawGrain(ctx, view, config, frame) {
    const amount = config.grain;
    if (amount <= 0.001) return;

    if (!this.grainTile) {
      this.grainTile = buildGrainTile(GRAIN_TILE_SIZE, mulberry32(0x9e3779b9));
    }

    // Deterministic per-frame jitter keeps the grain alive without flicker banding.
    const jitter = mulberry32(frame * 2654435761 + 1);
    const ox = Math.floor(jitter() * GRAIN_TILE_SIZE);
    const oy = Math.floor(jitter() * GRAIN_TILE_SIZE);
    const size = GRAIN_TILE_SIZE * Math.max(1, Math.round(view.unit));

    ctx.save();
    ctx.globalCompositeOperation = 'lighter';
    ctx.globalAlpha = clamp(amount * 0.16, 0, 1);
    ctx.imageSmoothingEnabled = false;
    for (let y = -oy; y < view.viewHeight; y += size) {
      for (let x = -ox; x < view.viewWidth; x += size) {
        ctx.drawImage(this.grainTile, x, y, size, size);
      }
    }
    ctx.restore();
  }

  /** Corner falloff, as every real lens produces. */
  drawVignette(ctx, view, config) {
    const amount = config.vignette;
    if (amount <= 0.001) return;

    const layer = this.vignetteLayer.get(
      [amount, view.width, view.height, view.viewX, view.viewY].join('|'),
      view.viewWidth,
      view.viewHeight,
      (lctx) => {
        lctx.translate(-view.viewX, -view.viewY);
        const cx = view.width * 0.5;
        const cy = view.height * 0.5;
        const radius = view.diag * 0.62;
        const gradient = lctx.createRadialGradient(cx, cy, radius * 0.32, cx, cy, radius);
        gradient.addColorStop(0, 'rgba(0,0,0,0)');
        gradient.addColorStop(0.62, 'rgba(0,0,0,' + (amount * 0.28).toFixed(4) + ')');
        gradient.addColorStop(1, 'rgba(0,0,0,' + (amount * 0.92).toFixed(4) + ')');
        lctx.fillStyle = gradient;
        lctx.fillRect(view.viewX, view.viewY, view.viewWidth, view.viewHeight);
      }
    );

    ctx.save();
    ctx.globalCompositeOperation = 'source-over';
    ctx.globalAlpha = 1;
    ctx.drawImage(layer, 0, 0);
    ctx.restore();
  }

  /** Drops every cached layer; call when the target size changes. */
  invalidate() {
    this.background.invalidate();
    this.vignetteLayer.invalidate();
    this.nebulaTextureKey = '';
  }
}

/**
 * Renders the fractal nebula clouds into a small offscreen canvas: over
 * black, or, `transparent`, as coloured haze whose opacity is its density.
 */
function buildNebulaTexture(config, aspect, transparent) {
  const width = aspect >= 1 ? NEBULA_TEXTURE_SIZE : Math.round(NEBULA_TEXTURE_SIZE * aspect);
  const height = aspect >= 1 ? Math.round(NEBULA_TEXTURE_SIZE / aspect) : NEBULA_TEXTURE_SIZE;

  const canvas = document.createElement('canvas');
  canvas.width = Math.max(2, width);
  canvas.height = Math.max(2, height);
  const ctx = canvas.getContext('2d');
  const image = ctx.createImageData(canvas.width, canvas.height);
  const data = image.data;

  const seed = (config.seed | 0) + 3301;
  const frequency = 2.6 / Math.max(0.05, config.nebulaScale);
  const hue = config.nebulaHue;

  for (let y = 0; y < canvas.height; y++) {
    const v = y / canvas.height;
    for (let x = 0; x < canvas.width; x++) {
      const u = x / canvas.width;

      // Two decorrelated fields: one shapes the clouds, one shifts their hue so
      // the haze is not a single flat colour.
      const cloud = fbm(u * frequency * aspect, v * frequency, seed, 5);
      const tint = fbm(u * frequency * 0.6 * aspect, v * frequency * 0.6, seed + 991, 3);

      // Threshold and sharpen so the clouds have edges instead of filling frame.
      const density = Math.pow(smoothstep(0.46, 0.95, cloud), 2.3);
      const rgb = hslToRgb(hue + (tint - 0.5) * 70, 0.34, 0.5);

      const i = (y * canvas.width + x) * 4;
      const shade = transparent ? 1 : density;
      data[i] = rgb[0] * shade;
      data[i + 1] = rgb[1] * shade;
      data[i + 2] = rgb[2] * shade;
      data[i + 3] = transparent ? 255 * density : 255;
    }
  }

  ctx.putImageData(image, 0, 0);
  return canvas;
}
