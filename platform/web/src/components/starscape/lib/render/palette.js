/**
 * Stellar colour palette.
 *
 * Stars are quantised into a small number of colour buckets so that the sprite
 * cache stays tiny while the sky still shows a believable spread of hues. The
 * quantisation is invisible at star scale — what the eye reads is the presence
 * of amber and blue-white amongst the white, not the exact temperature of any
 * individual point.
 */

import { applyColorVariation, blackbodyRGB, temperatureFromUnit } from '../core/color.js';
import { COLOR_BUCKETS } from './sprites.js';

/** Quantisation step for the colour-variation parameter, for cache stability. */
const VARIATION_STEPS = 16;

export class Palette {
  constructor(buckets = COLOR_BUCKETS) {
    this.buckets = buckets;
    this.cache = new Map();

    // Bucket centres in temperature space, computed once.
    this.base = new Array(buckets);
    for (let i = 0; i < buckets; i++) {
      const u = (i + 0.5) / buckets;
      this.base[i] = blackbodyRGB(temperatureFromUnit(u));
    }
  }

  /**
   * Colours for a given variation strength.
   *
   * @param {number} variation 0 = every star pure white, 1 = full black-body hue
   * @returns {{colors: Array<[number,number,number]>, key: string}}
   */
  forVariation(variation) {
    const step = Math.round(variation * VARIATION_STEPS) / VARIATION_STEPS;
    let entry = this.cache.get(step);
    if (!entry) {
      entry = {
        colors: this.base.map((rgb) => applyColorVariation(rgb, step)),
        key: 'v' + step,
      };
      this.cache.set(step, entry);
    }
    return entry;
  }

  /** Maps a unit random to a bucket index. */
  bucketOf(u) {
    const i = Math.floor(u * this.buckets);
    return i < 0 ? 0 : i >= this.buckets ? this.buckets - 1 : i;
  }
}
