/**
 * Star colour.
 *
 * Real stars are black-body radiators: their colour is a function of surface
 * temperature alone, running from deep orange (~2500 K) through white (~6500 K)
 * to blue-white (~15000 K). Rendering that hue range — rather than painting
 * every star pure white — is most of what sells a night sky as photographic.
 */

import { clamp, mix } from './noise.js';

/**
 * Black-body colour approximation (Tanner Helland's fit), returned normalised so
 * the brightest channel is always 255. Normalising keeps *luminance* under the
 * control of the brightness parameters and leaves this function responsible for
 * hue only.
 *
 * @param {number} kelvin  1000..40000
 * @returns {[number, number, number]} RGB in 0..255
 */
export function blackbodyRGB(kelvin) {
  const t = clamp(kelvin, 1000, 40000) / 100;
  let r;
  let g;
  let b;

  if (t <= 66) {
    r = 255;
    g = 99.4708025861 * Math.log(t) - 161.1195681661;
  } else {
    r = 329.698727446 * Math.pow(t - 60, -0.1332047592);
    g = 288.1221695283 * Math.pow(t - 60, -0.0755148492);
  }

  if (t >= 66) b = 255;
  else if (t <= 19) b = 0;
  else b = 138.5177312231 * Math.log(t - 10) - 305.0447927307;

  r = clamp(r, 0, 255);
  g = clamp(g, 0, 255);
  b = clamp(b, 0, 255);

  const peak = Math.max(r, g, b) || 1;
  return [(r / peak) * 255, (g / peak) * 255, (b / peak) * 255];
}

/**
 * Maps a uniform random value to a plausible stellar temperature.
 *
 * The curve is deliberately weighted toward the middle of the range: naked-eye
 * skies are dominated by near-white stars with a scattering of amber and
 * blue-white outliers, so a flat temperature distribution reads as fake.
 */
export function temperatureFromUnit(u) {
  const shaped = 0.5 + 0.5 * Math.sign(u - 0.5) * Math.pow(Math.abs(u - 0.5) * 2, 1.7);
  return mix(2900, 16000, shaped);
}

/** Desaturates a colour toward white; `variation` of 0 yields pure white. */
export function applyColorVariation(rgb, variation) {
  return [
    mix(255, rgb[0], variation),
    mix(255, rgb[1], variation),
    mix(255, rgb[2], variation),
  ];
}


/** HSL (h in degrees, s/l in 0..1) to RGB 0..255. */
export function hslToRgb(h, s, l) {
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const hp = (((h % 360) + 360) % 360) / 60;
  const x = c * (1 - Math.abs((hp % 2) - 1));
  let rgb;
  if (hp < 1) rgb = [c, x, 0];
  else if (hp < 2) rgb = [x, c, 0];
  else if (hp < 3) rgb = [0, c, x];
  else if (hp < 4) rgb = [0, x, c];
  else if (hp < 5) rgb = [x, 0, c];
  else rgb = [c, 0, x];
  const m = l - c / 2;
  return [(rgb[0] + m) * 255, (rgb[1] + m) * 255, (rgb[2] + m) * 255];
}
