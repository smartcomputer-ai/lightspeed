/**
 * Lightweight 2D value noise + fractal Brownian motion.
 *
 * Used for two things: clumping the background star field into loose galactic
 * bands (real skies are never uniform) and for the faint nebula haze.
 */

import { hash2 } from './rng.js';

const smooth = (t) => t * t * (3 - 2 * t);

/** Single octave of bilinearly interpolated value noise, in [0, 1]. */
export function valueNoise(x, y, seed = 0) {
  const xi = Math.floor(x);
  const yi = Math.floor(y);
  const xf = smooth(x - xi);
  const yf = smooth(y - yi);

  const n00 = hash2(xi, yi, seed);
  const n10 = hash2(xi + 1, yi, seed);
  const n01 = hash2(xi, yi + 1, seed);
  const n11 = hash2(xi + 1, yi + 1, seed);

  const top = n00 + (n10 - n00) * xf;
  const bottom = n01 + (n11 - n01) * xf;
  return top + (bottom - top) * yf;
}

/**
 * Fractal Brownian motion — layered value noise. Returns roughly [0, 1].
 *
 * @param {number} octaves  number of layers; each doubles frequency, halves gain
 */
export function fbm(x, y, seed = 0, octaves = 4, lacunarity = 2.07, gain = 0.5) {
  let amplitude = 1;
  let frequency = 1;
  let sum = 0;
  let norm = 0;

  for (let i = 0; i < octaves; i++) {
    sum += amplitude * valueNoise(x * frequency, y * frequency, seed + i * 1013);
    norm += amplitude;
    amplitude *= gain;
    frequency *= lacunarity;
  }
  return sum / norm;
}

/** Hermite ramp from 0 to 1 across [edge0, edge1]. */
export function smoothstep(edge0, edge1, x) {
  const t = Math.min(1, Math.max(0, (x - edge0) / (edge1 - edge0 || 1e-6)));
  return t * t * (3 - 2 * t);
}

export const clamp = (v, min, max) => (v < min ? min : v > max ? max : v);
export const mix = (a, b, t) => a + (b - a) * t;
