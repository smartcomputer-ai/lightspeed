/**
 * Composites a Scene onto a canvas.
 *
 * The renderer is resolution-agnostic: it is handed a target size and derives
 * every pixel measurement from it, so the same scene and config produce the same
 * picture at 1280x720 on screen and at 7680x4320 on export — just with more
 * detail. All sizes scale through `view.unit`, the frame diagonal expressed
 * relative to a 1920x1080 reference.
 *
 * The canvas may show only a window onto a larger frame (see `setViewport`):
 * the frame sets the composition and the scale of everything in it, the window
 * is the part actually drawn. Layers are drawn in frame coordinates under a
 * translation, and stars and streaks outside the window are skipped, so a
 * small window costs little however large its frame.
 *
 * Frame cost is kept low by caching the parts of the image that do not change
 * from frame to frame: the sky background, the vignette, and the faint majority
 * of the star field. During playback only the streaks, the bright twinkling
 * stars, bloom and grain are actually recomputed.
 */

import { SpriteCache } from './sprites.js';
import { Palette } from './palette.js';
import { Atmosphere } from './atmosphere.js';
import { BAND_ALL, BAND_STEADY, BAND_VARIABLE } from './starfield.js';

/** Frame diagonal that a `unit` of 1 corresponds to. */
const REFERENCE_DIAG = Math.hypot(1920, 1080);

/** Reusable offscreen canvas, so effect passes do not allocate per frame. */
class Scratch {
  constructor() {
    this.canvas = null;
  }

  acquire(width, height) {
    if (!this.canvas) this.canvas = document.createElement('canvas');
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width;
      this.canvas.height = height;
    }
    return this.canvas;
  }
}

export class Renderer {
  /**
   * @param {HTMLCanvasElement} [canvas] target; one is created if omitted
   * @param {{alpha?: boolean, transparentBackground?: boolean}} [options]
   *   `alpha` keeps the canvas transparent where the radial fade removes the
   *   sky; `transparentBackground` (which implies it) draws no sky, so the
   *   stars, streaks and nebula land on whatever is behind the canvas
   */
  constructor(canvas, options = {}) {
    this.canvas = canvas || document.createElement('canvas');
    /** Draws no sky: no black, airglow, vignette or grain; the nebula stays. */
    this.transparentBackground = Boolean(options.transparentBackground);
    this.ctx = this.canvas.getContext('2d', { alpha: Boolean(options.alpha) || this.transparentBackground });
    this.sprites = new SpriteCache();
    this.palette = new Palette();
    this.atmosphere = new Atmosphere();
    this.scratch = new Scratch();
    this.fieldLayer = null;
    this.fieldKey = '';
    this.lastCost = 0;
    /** Frame the canvas is a window onto; null means the canvas is the frame. */
    this.viewport = null;
    /** Radial fade around the vanishing point in canvas px, or null. */
    this.fade = null;
  }

  /** Sets the pixel dimensions of the target canvas, showing the whole frame. */
  resize(width, height) {
    const w = Math.max(1, Math.round(width));
    const h = Math.max(1, Math.round(height));
    const changed = this.viewport !== null || this.canvas.width !== w || this.canvas.height !== h;
    this.viewport = null;
    if (!changed) return false;
    this.canvas.width = w;
    this.canvas.height = h;
    this.invalidate();
    return true;
  }

  /**
   * Shows a window onto a larger frame: the frame is `frameWidth` by
   * `frameHeight` px, and the canvas becomes the `width` by `height` px region
   * whose top-left corner sits at (`x`, `y`) in it.
   */
  setViewport({ frameWidth, frameHeight, x, y, width, height }) {
    const next = {
      frameWidth: Math.max(1, Math.round(frameWidth)),
      frameHeight: Math.max(1, Math.round(frameHeight)),
      x: Math.round(x),
      y: Math.round(y),
      width: Math.max(1, Math.round(width)),
      height: Math.max(1, Math.round(height)),
    };
    const previous = this.viewport;
    const same =
      previous !== null &&
      Object.keys(next).every((key) => previous[key] === next[key]);
    if (same) return false;
    this.viewport = next;
    this.canvas.width = next.width;
    this.canvas.height = next.height;
    this.invalidate();
    return true;
  }

  /**
   * Fades the whole image to transparent with distance from the vanishing
   * point: fully visible within `start` px, gone beyond `end` px. Pass null to
   * turn it off. Needs a renderer created with `alpha: true`.
   */
  setFade(fade) {
    this.fade = fade && fade.end > 0 ? { start: Math.max(0, fade.start), end: fade.end } : null;
  }

  /** Frame geometry derived from the frame size and the vanishing point. */
  viewFor(config) {
    const viewport = this.viewport;
    const width = viewport ? viewport.frameWidth : this.canvas.width;
    const height = viewport ? viewport.frameHeight : this.canvas.height;
    const cx = config.centerX * width;
    const cy = config.centerY * height;
    const diag = Math.hypot(width, height);

    // Distance to the farthest frame corner: past this, nothing can be in the
    // frame. Streaks retire against the frame rather than the window, so a
    // window shows the same sky as the full frame would there.
    let cull = 0;
    for (const [x, y] of [[0, 0], [width, 0], [0, height], [width, height]]) {
      cull = Math.max(cull, Math.hypot(x - cx, y - cy));
    }

    return {
      width,
      height,
      cx,
      cy,
      diag,
      aspect: width / height,
      unit: diag / REFERENCE_DIAG,
      cullRadiusPx: cull,
      cullRadius: cull / diag,
      // The window the canvas shows, in frame px.
      viewX: viewport ? viewport.x : 0,
      viewY: viewport ? viewport.y : 0,
      viewWidth: this.canvas.width,
      viewHeight: this.canvas.height,
    };
  }

  /**
   * Draws one complete frame.
   *
   * @param {import('./scene.js').Scene} scene
   * @param {object} config
   */
  render(scene, config) {
    const started = performance.now();
    const ctx = this.ctx;
    const view = this.viewFor(config);
    const time = scene.timeAt(config);

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = 'source-over';

    // The sky doubles as the frame clear; transparent, it is the nebula alone.
    this.atmosphere.drawBackground(ctx, view, config, this.transparentBackground);
    this.drawField(ctx, scene, view, config, time);
    scene.streaks.draw(ctx, view, config, this.sprites, this.palette);

    // Bloom reads the composited light, so it must run after both star layers
    // but before the camera artefacts that sit on top of the whole image.
    this.atmosphere.drawBloom(ctx, view, config, this.scratch);
    // Grain and vignette treat the sky as a photograph; without a sky they
    // would only lay noise and shadow over the page.
    if (!this.transparentBackground) {
      this.atmosphere.drawGrain(ctx, view, config, scene.frame);
      this.atmosphere.drawVignette(ctx, view, config);
    }
    this.drawFade(ctx, view);

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = 'source-over';
    this.lastCost = performance.now() - started;
  }

  /**
   * Draws the background stars in two passes.
   *
   * The faint majority never varies within a frame sequence, so it is cached and
   * blitted. When twinkling is on, only the bright minority above the twinkle
   * magnitude is redrawn live — which is the only part of the field where the
   * variation is visible anyway.
   */
  drawField(ctx, scene, view, config, time) {
    const twinkling = config.starTwinkle > 0 && config.twinkleSpeed > 0;
    const band = twinkling ? BAND_STEADY : BAND_ALL;
    const key = [
      scene.signature,
      band,
      config.starSize,
      config.starBrightness,
      config.starSizeVar,
      config.starColorVar,
      config.starSpikes,
      config.starSpikeSize,
      config.exposure,
      view.width,
      view.height,
      view.viewX,
      view.viewY,
      view.viewWidth,
      view.viewHeight,
    ].join('|');

    if (this.fieldKey !== key || !this.fieldLayer) {
      if (!this.fieldLayer) this.fieldLayer = document.createElement('canvas');
      if (this.fieldLayer.width !== view.viewWidth || this.fieldLayer.height !== view.viewHeight) {
        this.fieldLayer.width = view.viewWidth;
        this.fieldLayer.height = view.viewHeight;
      }
      const lctx = this.fieldLayer.getContext('2d');
      lctx.setTransform(1, 0, 0, 1, 0, 0);
      lctx.clearRect(0, 0, view.viewWidth, view.viewHeight);
      lctx.translate(-view.viewX, -view.viewY);
      scene.field.draw(lctx, view, config, this.sprites, this.palette, time, band);
      lctx.setTransform(1, 0, 0, 1, 0, 0);
      this.fieldKey = key;
    }

    ctx.save();
    ctx.globalCompositeOperation = 'lighter';
    ctx.globalAlpha = 1;
    ctx.drawImage(this.fieldLayer, 0, 0);
    ctx.restore();

    if (twinkling) {
      ctx.save();
      ctx.translate(-view.viewX, -view.viewY);
      scene.field.draw(ctx, view, config, this.sprites, this.palette, time, BAND_VARIABLE);
      ctx.restore();
    }
  }

  /** Masks the finished frame with the radial fade, if one is set. */
  drawFade(ctx, view) {
    const fade = this.fade;
    if (!fade) return;
    const cx = view.cx - view.viewX;
    const cy = view.cy - view.viewY;
    const start = Math.min(fade.start, fade.end - 0.001);
    const gradient = ctx.createRadialGradient(cx, cy, Math.max(0, start), cx, cy, fade.end);
    // Smoothstep-shaped stops, so the edge of the fade never reads as a ring.
    for (let i = 0; i <= 8; i++) {
      const t = i / 8;
      gradient.addColorStop(t, 'rgba(0,0,0,' + (1 - t * t * (3 - 2 * t)).toFixed(4) + ')');
    }
    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = 'destination-in';
    ctx.fillStyle = gradient;
    ctx.fillRect(0, 0, view.viewWidth, view.viewHeight);
    ctx.restore();
  }

  /** Discards every cached layer. */
  invalidate() {
    this.fieldKey = '';
    this.atmosphere.invalidate();
  }
}
