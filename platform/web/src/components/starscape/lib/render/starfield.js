/**
 * The background star field — the static sky the warp streaks travel through.
 *
 * Realism here comes from four things that a naive random-dots field lacks:
 *
 *  1. A magnitude distribution that follows a power law, so the sky is mostly
 *     faint pinpricks with a handful of dominant stars, not an even sprinkle.
 *  2. Clustering driven by fractal noise, mimicking the way real star density
 *     bands and voids across the sky.
 *  3. A physically shaped point spread (core plus halo) instead of flat dots.
 *  4. Diffraction spikes on the brightest few, as a real lens would record.
 *
 * Star attributes are stored as normalised unit randoms in typed arrays. Every
 * visual property is derived from them at draw time, so changing size, colour or
 * brightness never requires regenerating the field.
 */

import { Rng } from '../core/rng.js';
import { clamp, fbm, mix } from '../core/noise.js';
import { STAR_SPRITE_SCALE, pickMip } from './sprites.js';

/** Rejection-sampling attempts before a star is placed wherever it landed. */
const MAX_PLACEMENT_TRIES = 6;

/**
 * Magnitude above which a star is treated as twinkling.
 *
 * Scintillation is only perceptible on stars bright enough for the eye to track
 * their variation, so the faint majority can be drawn once into a cached layer
 * and left alone. With the power-law magnitude distribution this leaves roughly
 * a fifth of the field to redraw per animated frame.
 */
export const TWINKLE_MAGNITUDE = 0.45;

/** Which slice of the field to draw. See {@link StarField#draw}. */
export const BAND_ALL = 0;
export const BAND_STEADY = 1;
export const BAND_VARIABLE = 2;

export class StarField {
  constructor() {
    this.count = 0;
    this.x = new Float32Array(0);
    this.y = new Float32Array(0);
    this.magnitude = new Float32Array(0);
    this.temperature = new Float32Array(0);
    this.phase = new Float32Array(0);
    this.flicker = new Float32Array(0);
  }

  /**
   * Regenerates the field. Only called when seed, count or clustering change.
   *
   * @param {object} config
   */
  generate(config) {
    const count = Math.max(0, Math.round(config.starCount));
    const rng = new Rng((config.seed | 0) ^ 0x5f3a7c11);

    this.count = count;
    this.x = new Float32Array(count);
    this.y = new Float32Array(count);
    this.magnitude = new Float32Array(count);
    this.temperature = new Float32Array(count);
    this.phase = new Float32Array(count);
    this.flicker = new Float32Array(count);

    const clustering = config.starClustering;
    const noiseSeed = (config.seed | 0) + 7717;

    for (let i = 0; i < count; i++) {
      let px = rng.float();
      let py = rng.float();

      // Rejection-sample against a fractal density field so stars gather into
      // loose drifts and leave voids, the way a real sky does.
      if (clustering > 0) {
        for (let attempt = 0; attempt < MAX_PLACEMENT_TRIES; attempt++) {
          const density = fbm(px * 3.1, py * 3.1, noiseSeed, 4);
          // Blend the noise toward flat so clustering stays a dial, not a switch.
          const accept = mix(1, density * density * 1.9, clustering);
          if (rng.float() < accept) break;
          px = rng.float();
          py = rng.float();
        }
      }

      this.x[i] = px;
      this.y[i] = py;
      // Power law: the great majority of stars sit near the faint end.
      this.magnitude[i] = rng.power(3.1);
      this.temperature[i] = rng.float();
      this.phase[i] = rng.float();
      this.flicker[i] = 0.35 + rng.float() * 0.85;
    }
  }

  /**
   * Draws the field additively into `ctx`.
   *
   * @param {CanvasRenderingContext2D} ctx
   * @param {{width:number,height:number,unit:number,viewX?:number,viewY?:number,viewWidth?:number,viewHeight?:number}} view
   *   frame geometry; stars outside the view window are skipped
   * @param {object} config
   * @param {import('./sprites.js').SpriteCache} sprites
   * @param {import('./palette.js').Palette} palette
   * @param {number} time  seconds, drives twinkle
   * @param {number} band  BAND_ALL, or BAND_STEADY / BAND_VARIABLE to draw only
   *                       the stars below / above the twinkle magnitude
   */
  draw(ctx, view, config, sprites, palette, time, band = BAND_ALL) {
    if (this.count === 0 || config.starBrightness <= 0) return;

    const { colors, key: paletteKey } = palette.forVariation(config.starColorVar);
    const exposure = config.exposure;
    const baseSize = config.starSize * view.unit;
    const sizeVar = config.starSizeVar;
    const twinkle = config.starTwinkle;
    const twinklePhase = time * config.twinkleSpeed * 2.4;

    // Diffraction spikes are reserved for the top slice of the magnitude range;
    // the parameter widens that slice rather than dimming the spikes.
    const spikeCutoff = 1 - config.starSpikes * 0.22;
    const spikeSize = config.starSpikeSize;
    const showSpikes = config.starSpikes > 0 && spikeSize > 0;

    // The window the canvas shows, in frame px (the whole frame by default).
    const left = view.viewX ?? 0;
    const top = view.viewY ?? 0;
    const right = left + (view.viewWidth ?? view.width);
    const bottom = top + (view.viewHeight ?? view.height);

    ctx.save();
    ctx.globalCompositeOperation = 'lighter';

    for (let i = 0; i < this.count; i++) {
      const magnitude = this.magnitude[i];

      if (band === BAND_STEADY && magnitude >= TWINKLE_MAGNITUDE) continue;
      if (band === BAND_VARIABLE && magnitude < TWINKLE_MAGNITUDE) continue;

      // Brighter stars are both more luminous and physically larger on film.
      let brightness = (0.1 + magnitude * 0.9) * config.starBrightness * exposure;
      if (twinkle > 0 && magnitude >= TWINKLE_MAGNITUDE) {
        const t = Math.sin(twinklePhase * this.flicker[i] + this.phase[i] * 6.283);
        brightness *= 1 + t * twinkle * 0.55;
      }
      if (brightness <= 0.004) continue;

      const size = baseSize * mix(1, 0.35 + magnitude * 1.9, sizeVar);
      const sprite = size * STAR_SPRITE_SCALE;
      if (sprite < 0.5) continue;

      const px = this.x[i] * view.width;
      const py = this.y[i] * view.height;
      // Skip stars outside the window the canvas shows, spikes included.
      const reach = Math.max(sprite * 0.5, showSpikes ? size * spikeSize * 13 : 0);
      if (px + reach < left || px - reach > right || py + reach < top || py - reach > bottom) continue;
      const bucket = palette.bucketOf(this.temperature[i]);
      const rgb = colors[bucket];
      const id = paletteKey + ':' + bucket;

      const mip = pickMip(sprites.star(id, rgb), sprite);
      const half = sprite * 0.5;
      ctx.globalAlpha = clamp(brightness, 0, 1);
      ctx.drawImage(mip, px - half, py - half, sprite, sprite);

      // A second additive pass carries brightness past the 8-bit alpha ceiling,
      // letting the very brightest stars blow out their cores like real ones.
      if (brightness > 1) {
        ctx.globalAlpha = clamp(brightness - 1, 0, 1);
        ctx.drawImage(mip, px - half, py - half, sprite, sprite);
      }

      if (showSpikes && magnitude > spikeCutoff) {
        const strength = (magnitude - spikeCutoff) / (1 - spikeCutoff);
        const span = size * spikeSize * 26 * strength;
        if (span > 2) {
          ctx.globalAlpha = clamp(brightness * strength * 0.75, 0, 1);
          const spikeHalf = span * 0.5;
          ctx.drawImage(
            sprites.spike(id, rgb),
            px - spikeHalf,
            py - spikeHalf,
            span,
            span
          );
        }
      }
    }

    ctx.restore();
  }
}
