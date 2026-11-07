/**
 * Warp streaks — stars smeared into trails by forward motion at speed.
 *
 * The geometry is derived from an actual perspective projection rather than
 * being drawn as decorative rays, which is what makes the result read as motion
 * instead of as a starburst graphic.
 *
 * A star sits at depth `z` with a transverse offset `a`, so it projects to a
 * screen radius `r = a / z` along a fixed bearing from the vanishing point. Over
 * one exposure the camera advances by `E`, so the star was at depth `z + E` when
 * the shutter opened. The trail therefore runs from
 *
 *     r_tail = a / (z + E)      to      r_head = a / z
 *
 * giving a length of `r_head * E / (z + E)`. Three properties fall out of this
 * for free, and all three are visible in real warp photography:
 *
 *  - trails are exactly radial and dead straight;
 *  - they grow with distance from the vanishing point, so the centre stays a
 *    tight knot of short marks while the edges show long rakes;
 *  - a trail can never reach past the vanishing point, however fast you go.
 *
 * Animation is the same model advanced in time: `z` decreases, the star sweeps
 * outward and its trail stretches, exactly as it would from the cockpit.
 */

import { Rng } from '../core/rng.js';
import { clamp, mix, smoothstep } from '../core/noise.js';
import {
  STAR_SPRITE_SCALE,
  STREAK_HEIGHT_SCALE,
  pickMip,
  streakSpriteHeight,
} from './sprites.js';

/** Depth of the far plane. Near plane is the `depth` parameter. */
const Z_FAR = 1;

/** Camera advance per exposure at speed 1, in depth units. */
const EXPOSURE_REF = 0.06;

/** Camera velocity at flow speed 1, in depth units per second. */
const FLOW_REF = 0.12;

/** Trail length at which the energy-falloff term is neutral, in reference px. */
const ENERGY_REF_LENGTH = 26;

/** Fraction of the depth range over which a recycled star fades back in. */
const FADE_IN_DEPTH = 0.09;

export class StreakField {
  constructor() {
    this.count = 0;
    this.angle = new Float32Array(0);
    this.offset = new Float32Array(0);
    this.z = new Float32Array(0);
    this.luminosity = new Float32Array(0);
    this.temperature = new Float32Array(0);
    this.thickness = new Float32Array(0);
    this.rng = new Rng(1);
    this.spawnRadius = 0.5;
    this.zNear = 0.06;
    this.spreadPower = 2;
  }

  /**
   * Regenerates the streak population in a steady-state configuration, so the
   * very first frame already looks like a scene in motion rather than one that
   * has just started.
   */
  generate(config) {
    const count = Math.max(0, Math.round(config.streakCount));
    const rng = new Rng((config.seed | 0) ^ 0x1d3b5f77);
    this.rng = rng;
    this.count = count;

    this.angle = new Float32Array(count);
    this.offset = new Float32Array(count);
    this.z = new Float32Array(count);
    this.luminosity = new Float32Array(count);
    this.temperature = new Float32Array(count);
    this.thickness = new Float32Array(count);

    this.zNear = Math.min(config.depth, Z_FAR * 0.8);
    this.spawnRadius = spawnRadiusFor(config);
    // Spread 0 packs everything against the vanishing point; 1 spreads stars
    // evenly across the frame (an exponent of 0.5 is uniform over a disc).
    this.spreadPower = mix(3, 0.5, config.streakSpread);

    for (let i = 0; i < count; i++) {
      // Depth is uniform across the range: a star travelling at constant speed
      // spends equal time in equal slices of depth, so this *is* the steady state.
      this.z[i] = mix(this.zNear, Z_FAR, rng.float());
      this.angle[i] = rng.float() * Math.PI * 2;
      this.offset[i] = this.sampleRadius(rng) * this.z[i];
      this.luminosity[i] = rng.power(2.4);
      this.temperature[i] = rng.float();
      this.thickness[i] = rng.float();
    }
  }

  /** Screen radius (normalised to the frame diagonal) for a newly placed star. */
  sampleRadius(rng) {
    return this.spawnRadius * rng.power(this.spreadPower);
  }

  /** Re-seeds one star at the far plane, where it fades in unnoticed. */
  respawn(i) {
    const rng = this.rng;
    this.z[i] = Z_FAR;
    this.angle[i] = rng.float() * Math.PI * 2;
    this.offset[i] = this.sampleRadius(rng) * Z_FAR;
    this.luminosity[i] = rng.power(2.4);
    this.temperature[i] = rng.float();
    this.thickness[i] = rng.float();
  }

  /**
   * Advances the simulation by `dt` seconds.
   *
   * @param {number} dt
   * @param {object} config
   * @param {number} cullRadius  normalised radius past which a trail is fully
   *                             outside the frame and the star can be reused
   */
  step(dt, config, cullRadius) {
    const velocity = config.flowSpeed * FLOW_REF;
    if (velocity <= 0 || dt <= 0) return;

    const exposure = config.streakSpeed * EXPOSURE_REF;
    const travel = velocity * dt;

    for (let i = 0; i < this.count; i++) {
      const z = this.z[i] - travel;
      if (z <= this.zNear) {
        this.respawn(i);
        continue;
      }
      this.z[i] = z;
      // Retire the star only once its *tail* has left the frame; culling on the
      // head would clip trails that are still half on screen.
      if (this.offset[i] / (z + exposure) > cullRadius) this.respawn(i);
    }
  }

  /**
   * Draws every trail additively into `ctx`.
   *
   * @param {CanvasRenderingContext2D} ctx
   * @param {object} view  frame geometry from the renderer
   * @param {object} config
   * @param {import('./sprites.js').SpriteCache} sprites
   * @param {import('./palette.js').Palette} palette
   */
  draw(ctx, view, config, sprites, palette) {
    if (this.count === 0 || config.streakBrightness <= 0) return;

    const { colors, key: paletteKey } = palette.forVariation(config.streakColorVar);
    const hot = palette.forVariation(1);
    const warmRgb = hot.colors[0];
    const coolRgb = hot.colors[hot.colors.length - 1];
    const warmId = hot.key + ':0';
    const coolId = hot.key + ':' + (hot.colors.length - 1);

    const exposure = config.streakSpeed * EXPOSURE_REF;
    const softness = config.streakSoftness;
    const tailFade = config.streakTailFade;
    const diag = view.diag;
    const unit = view.unit;
    const baseThickness = config.streakThickness * unit;
    const spriteHeight = streakSpriteHeight(baseThickness);
    const energyRef = ENERGY_REF_LENGTH * unit;
    const energyPower = config.streakEnergy * 0.55;
    const maxLength = config.streakMaxLength * diag;
    const gain = config.streakBrightness * config.exposure;
    const headGlow = config.streakHeadGlow;
    const chroma = config.streakChroma;
    const fadeStart = Z_FAR - (Z_FAR - this.zNear) * FADE_IN_DEPTH;
    // The window the canvas shows, in frame px (the whole frame by default).
    const left = view.viewX ?? 0;
    const top = view.viewY ?? 0;
    const right = left + (view.viewWidth ?? view.width);
    const bottom = top + (view.viewHeight ?? view.height);

    ctx.save();
    ctx.globalCompositeOperation = 'lighter';

    for (let i = 0; i < this.count; i++) {
      const z = this.z[i];
      const offset = this.offset[i];
      const headRadius = (offset / z) * diag;
      const tailRadius = (offset / (z + exposure)) * diag;
      if (tailRadius > view.cullRadiusPx) continue;

      const luminosity = this.luminosity[i];
      let brightness = (0.12 + luminosity * 0.88) * gain;

      // A trail spreads one star's light over many pixels, so the longer it is
      // the dimmer it reads. This is the single biggest cue that the streaks
      // belong to the same sky as the points around them.
      let length = clamp(headRadius - tailRadius, 0, maxLength);
      if (energyPower > 0 && length > energyRef) {
        brightness *= Math.pow(energyRef / length, energyPower);
      }

      // Hide the moment a recycled star reappears at the far plane.
      if (z > fadeStart) brightness *= smoothstep(Z_FAR, fadeStart, z);
      if (brightness <= 0.004) continue;

      const thickness =
        baseThickness * (0.75 + this.thickness[i] * 0.5) * (0.85 + luminosity * 0.4);
      const spriteScale = thickness * STREAK_HEIGHT_SCALE;
      length = Math.max(length, thickness * 0.9);

      const angle = this.angle[i];
      const cos = Math.cos(angle);
      const sin = Math.sin(angle);

      // Skip trails that miss the window the canvas shows: test the trail's
      // bounding box, widened by its glow, fringe and sprite height.
      const glowSize = headGlow > 0 ? thickness * headGlow * STAR_SPRITE_SCALE * 0.55 : 0;
      const pad = Math.max(spriteScale, glowSize) * 0.5 + chroma * 1.8 * unit * 1.6;
      const x0 = view.cx + cos * tailRadius;
      const y0 = view.cy + sin * tailRadius;
      const x1 = x0 + cos * length;
      const y1 = y0 + sin * length;
      if (
        Math.max(x0, x1) + pad < left ||
        Math.min(x0, x1) - pad > right ||
        Math.max(y0, y1) + pad < top ||
        Math.min(y0, y1) - pad > bottom
      ) {
        continue;
      }

      const bucket = palette.bucketOf(this.temperature[i]);
      const rgb = colors[bucket];
      const id = paletteKey + ':' + bucket;
      const body = sprites.streak(id, rgb, softness, tailFade, spriteHeight);

      // Local space: origin at the tail, +x pointing outward along the trail,
      // shifted so the window's corner lands at the canvas origin.
      ctx.setTransform(cos, sin, -sin, cos, x0 - left, y0 - top);

      const spriteTop = -spriteScale * 0.5;
      const alpha = clamp(brightness, 0, 1);

      // Chromatic fringing: a real fast lens splits colour radially toward the
      // edge of frame, so the effect scales with distance from the centre.
      if (chroma > 0 && headRadius > diag * 0.12) {
        const shift = chroma * 1.8 * unit * clamp(headRadius / (diag * 0.5), 0, 1.6);
        const fringe = alpha * 0.45;
        ctx.globalAlpha = fringe;
        ctx.drawImage(
          sprites.streak(warmId, warmRgb, softness, tailFade, spriteHeight),
          -shift,
          spriteTop,
          length,
          spriteScale
        );
        ctx.drawImage(
          sprites.streak(coolId, coolRgb, softness, tailFade, spriteHeight),
          shift,
          spriteTop,
          length,
          spriteScale
        );
      }

      ctx.globalAlpha = alpha;
      ctx.drawImage(body, 0, spriteTop, length, spriteScale);
      if (brightness > 1) {
        ctx.globalAlpha = clamp(brightness - 1, 0, 1);
        ctx.drawImage(body, 0, spriteTop, length, spriteScale);
      }

      // The leading point of the trail is the star itself, caught at the end of
      // the exposure — brighter and rounder than the smear behind it.
      if (headGlow > 0) {
        const glow = thickness * headGlow * STAR_SPRITE_SCALE * 0.55;
        if (glow > 1) {
          ctx.globalAlpha = clamp(brightness * (0.55 + headGlow * 0.3), 0, 1);
          ctx.drawImage(
            pickMip(sprites.star(id, rgb), glow),
            length - glow * 0.5,
            -glow * 0.5,
            glow,
            glow
          );
        }
      }
    }

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.restore();
  }
}

/**
 * Largest normalised radius a star may spawn at. Kept aspect-independent so that
 * resizing the window never reshuffles the composition; the renderer culls
 * against the real frame instead.
 */
function spawnRadiusFor(config) {
  const offCentre = Math.max(
    Math.abs(config.centerX - 0.5),
    Math.abs(config.centerY - 0.5)
  );
  return (0.5 + offCentre * 1.2) * 1.05;
}
