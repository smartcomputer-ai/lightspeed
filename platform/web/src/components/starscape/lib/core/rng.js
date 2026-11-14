/**
 * Deterministic pseudo-random number generation.
 *
 * Every visual element of the starscape is derived from a single integer seed so
 * that a given set of parameters always reproduces the exact same image — which
 * is what makes the tool usable for design work and for high-resolution export.
 */

/** mulberry32 — small, fast, statistically decent 32-bit PRNG. */
export function mulberry32(seed) {
  let a = seed >>> 0;
  return function next() {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Convenience wrapper with the sampling helpers the generators need. */
export class Rng {
  constructor(seed = 1) {
    this.next = mulberry32(seed);
  }

  /** Uniform in [0, 1). */
  float() {
    return this.next();
  }

  /**
   * Uniform in [0,1) shaped by an exponent: `power > 1` biases toward 0,
   * `power < 1` biases toward 1.
   */
  power(power) {
    return Math.pow(this.next(), power);
  }
}

/** Integer hash used by the value-noise field. */
export function hash2(x, y, seed) {
  let h = Math.imul(x | 0, 0x27d4eb2d) ^ Math.imul(y | 0, 0x165667b1) ^ (seed | 0);
  h = Math.imul(h ^ (h >>> 15), 0x2c1b3c6d);
  h = Math.imul(h ^ (h >>> 12), 0x297a2d39);
  return ((h ^ (h >>> 15)) >>> 0) / 4294967296;
}
