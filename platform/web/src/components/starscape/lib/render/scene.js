/**
 * The generated contents of the sky.
 *
 * Kept deliberately separate from the renderer: the scene holds resolution-
 * independent data, the renderer turns it into pixels. That split is what lets
 * a PNG export at 4x resolution show the exact same sky, at the exact same
 * animation frame, as the canvas on screen.
 */

import { StarField } from './starfield.js';
import { StreakField } from './streaks.js';
import { REBUILD_KEYS } from '../core/config.js';

export class Scene {
  constructor(config) {
    this.field = new StarField();
    this.streaks = new StreakField();
    this.frame = 0;
    this.signature = '';
    this.rebuild(config);
  }

  /** Regenerates star and streak populations from the config's seed. */
  rebuild(config) {
    this.field.generate(config);
    this.streaks.generate(config);
    this.signature = signatureOf(config);
    this.frame = 0;
  }

  /** Rebuilds only if a generation-affecting parameter actually changed. */
  syncTo(config) {
    const next = signatureOf(config);
    if (next !== this.signature) this.rebuild(config);
  }

  /**
   * Advances one fixed-length frame. A fixed timestep — rather than wall-clock
   * delta — means frame N always looks identical, which is what makes stepping,
   * scrubbing and future frame-sequence export reproducible.
   */
  advance(config, cullRadius) {
    this.streaks.step(1 / config.fps, config, cullRadius);
    this.frame++;
  }

  /** Elapsed time in seconds at the current frame, for time-based effects. */
  timeAt(config) {
    return this.frame / Math.max(1, config.fps);
  }
}

/** Concatenated values of every parameter that changes what gets generated. */
function signatureOf(config) {
  const parts = [];
  for (const key of REBUILD_KEYS) parts.push(config[key]);
  return parts.join('|');
}
