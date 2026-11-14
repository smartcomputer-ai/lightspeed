/**
 * High-level API for embedding the starscape renderer in another website.
 *
 * Animation timing remains with the host so frameworks and existing render
 * loops can decide exactly when work happens.
 */

import { createDefaultConfig, PARAMS } from './core/config.js';
import { Renderer } from './render/renderer.js';
import { Scene } from './render/scene.js';

export class Starscape {
  /**
   * @param {HTMLCanvasElement} canvas
   * @param {object} [overrides] renderer parameters overriding the defaults
   * @param {{alpha?: boolean, transparentBackground?: boolean}} [options]
   *   `alpha` gives the canvas transparency, which the radial fade needs;
   *   `transparentBackground` draws the stars, streaks and nebula, no sky
   */
  constructor(canvas, overrides = {}, options = {}) {
    if (!canvas || typeof canvas.getContext !== 'function') {
      throw new TypeError('Starscape requires a canvas element.');
    }

    this.config = createDefaultConfig();
    Object.assign(this.config, validatePatch(overrides));
    this.renderer = new Renderer(canvas, options);
    this.scene = new Scene(this.config);
  }

  get canvas() {
    return this.renderer.canvas;
  }

  get frame() {
    return this.scene.frame;
  }

  /** Sets the canvas backing resolution. */
  resize(width, height) {
    this.renderer.resize(width, height);
    return this;
  }

  /**
   * Shows a window onto a larger frame; see `Renderer#setViewport`. The frame
   * sets the composition and scale, the canvas covers only the window.
   */
  setViewport(viewport) {
    this.renderer.setViewport(viewport);
    return this;
  }

  /** Fades the image out between two radii from the vanishing point, in canvas px. */
  setFade(fade) {
    this.renderer.setFade(fade);
    return this;
  }

  /** Applies a validated parameter patch, rebuilding generated data if needed. */
  update(patch) {
    Object.assign(this.config, validatePatch(patch));
    this.scene.syncTo(this.config);
    return this;
  }

  /** Restores defaults plus optional overrides and returns to frame zero. */
  reset(overrides = {}) {
    const next = createDefaultConfig();
    Object.assign(next, validatePatch(overrides));
    Object.assign(this.config, next);
    this.scene.rebuild(this.config);
    return this;
  }

  /** Advances one or more deterministic fixed-timestep frames. */
  advance(frames = 1) {
    if (!Number.isInteger(frames) || frames < 0) {
      throw new RangeError('Frame count must be a non-negative integer.');
    }

    const cullRadius = this.renderer.viewFor(this.config).cullRadius;
    for (let i = 0; i < frames; i++) {
      this.scene.advance(this.config, cullRadius);
    }
    return this;
  }

  /** Regenerates the initial deterministic frame. */
  rewind() {
    this.scene.rebuild(this.config);
    return this;
  }

  /** Draws the current frame onto the attached canvas. */
  render() {
    this.renderer.render(this.scene, this.config);
    return this;
  }
}

function validatePatch(patch) {
  if (patch === null || typeof patch !== 'object' || Array.isArray(patch)) {
    throw new TypeError('Configuration must be an object.');
  }

  const validated = {};
  for (const [key, value] of Object.entries(patch)) {
    const param = PARAMS.get(key);
    if (!param) throw new RangeError('Unknown parameter: ' + key);
    if (typeof value !== 'number' || !Number.isFinite(value)) {
      throw new TypeError(key + ' must be a finite number.');
    }
    if (value < param.min || value > param.max) {
      throw new RangeError(key + ' must be between ' + param.min + ' and ' + param.max + '.');
    }
    validated[key] = value;
  }
  return validated;
}
