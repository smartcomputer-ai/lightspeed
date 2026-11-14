/**
 * Parameter schema.
 *
 * A single declarative list drives both the default config and the generated
 * control panel. Adding a knob is a one-line change here and nowhere else.
 *
 * `rebuild` marks parameters that change the *generated data* (how many stars
 * exist, where they are) rather than merely how it is drawn. Only those trigger
 * a scene regeneration; everything else is applied at draw time, which is what
 * keeps slider dragging smooth even with tens of thousands of stars.
 */

export const SCHEMA = [
  {
    id: 'composition',
    label: 'Composition',
    open: true,
    params: [
      { key: 'seed', label: 'Seed', type: 'seed', min: 0, max: 999999, step: 1, value: 972191, rebuild: true },
      { key: 'centerX', label: 'Vanishing point X', min: -0.5, max: 1.5, step: 0.001, value: 0.5, rebuild: true },
      { key: 'centerY', label: 'Vanishing point Y', min: -0.5, max: 1.5, step: 0.001, value: 0.5, rebuild: true },
      { key: 'depth', label: 'Depth range', min: 0.02, max: 0.6, step: 0.005, value: 0.06, rebuild: true },
    ],
  },
  {
    id: 'stars',
    label: 'Stars',
    open: true,
    params: [
      { key: 'starCount', label: 'Count', min: 0, max: 30000, step: 50, value: 11350, rebuild: true },
      { key: 'starSize', label: 'Size', min: 0.2, max: 6, step: 0.01, value: 1.15 },
      { key: 'starBrightness', label: 'Brightness', min: 0, max: 2, step: 0.01, value: 1 },
      { key: 'starSizeVar', label: 'Size variation', min: 0, max: 1, step: 0.01, value: 0.78 },
      { key: 'starColorVar', label: 'Colour variation', min: 0, max: 1, step: 0.01, value: 0.4 },
      { key: 'starClustering', label: 'Clustering', min: 0, max: 1, step: 0.01, value: 0.45, rebuild: true },
      { key: 'starSpikes', label: 'Diffraction spikes', min: 0, max: 1, step: 0.01, value: 0.18 },
      { key: 'starSpikeSize', label: 'Spike length', min: 0, max: 3, step: 0.01, value: 1 },
      { key: 'starTwinkle', label: 'Twinkle', min: 0, max: 1, step: 0.01, value: 0.55 },
    ],
  },
  {
    id: 'streaks',
    label: 'Warp streaks',
    open: true,
    params: [
      { key: 'streakCount', label: 'Count', min: 0, max: 4000, step: 10, value: 480, rebuild: true },
      { key: 'streakSpeed', label: 'Speed (trail length)', min: 0, max: 4, step: 0.01, value: 3.11 },
      { key: 'streakSpread', label: 'Spread from centre', min: 0, max: 1, step: 0.01, value: 0.45, rebuild: true },
      { key: 'streakThickness', label: 'Thickness', min: 0.2, max: 8, step: 0.01, value: 1.55 },
      { key: 'streakBrightness', label: 'Brightness', min: 0, max: 2, step: 0.01, value: 1.33 },
      { key: 'streakSoftness', label: 'Softness', min: 0, max: 1, step: 0.01, value: 0.45 },
      { key: 'streakTailFade', label: 'Tail fade', min: 0, max: 1, step: 0.01, value: 0.93 },
      { key: 'streakHeadGlow', label: 'Head glow', min: 0, max: 3, step: 0.01, value: 2.91 },
      { key: 'streakEnergy', label: 'Energy falloff', min: 0, max: 1, step: 0.01, value: 0.48 },
      { key: 'streakColorVar', label: 'Colour variation', min: 0, max: 1, step: 0.01, value: 0.49 },
      { key: 'streakChroma', label: 'Chromatic fringe', min: 0, max: 1, step: 0.01, value: 0.18 },
      { key: 'streakMaxLength', label: 'Max length', min: 0.02, max: 1.5, step: 0.01, value: 0.7 },
    ],
  },
  {
    id: 'atmosphere',
    label: 'Atmosphere',
    open: false,
    params: [
      { key: 'nebulaAmount', label: 'Nebula haze', min: 0, max: 1, step: 0.01, value: 0.85 },
      { key: 'nebulaScale', label: 'Nebula scale', min: 0.2, max: 4, step: 0.01, value: 1.02 },
      { key: 'nebulaHue', label: 'Nebula hue', min: 0, max: 360, step: 1, value: 222, unit: '°' },
      { key: 'airglow', label: 'Airglow', min: 0, max: 1, step: 0.01, value: 0.1 },
      { key: 'bloomAmount', label: 'Bloom', min: 0, max: 1, step: 0.01, value: 0.35 },
      { key: 'bloomRadius', label: 'Bloom radius', min: 0, max: 60, step: 0.5, value: 18 },
      { key: 'vignette', label: 'Vignette', min: 0, max: 1, step: 0.01, value: 0 },
      { key: 'grain', label: 'Film grain', min: 0, max: 1, step: 0.01, value: 0.22 },
      { key: 'exposure', label: 'Exposure', min: 0.2, max: 2.5, step: 0.01, value: 1 },
    ],
  },
  {
    id: 'animation',
    label: 'Animation',
    open: false,
    params: [
      { key: 'flowSpeed', label: 'Flow speed', min: 0, max: 4, step: 0.01, value: 1 },
      { key: 'twinkleSpeed', label: 'Twinkle speed', min: 0, max: 4, step: 0.01, value: 1 },
      { key: 'fps', label: 'Frames per second', min: 6, max: 60, step: 1, value: 30 },
    ],
  },
];

/** Flat map of every parameter definition, keyed by parameter name. */
export const PARAMS = new Map(
  SCHEMA.flatMap((group) => group.params.map((p) => [p.key, p]))
);

/** Parameter names that require the scene data to be regenerated when changed. */
export const REBUILD_KEYS = new Set(
  [...PARAMS.values()].filter((p) => p.rebuild).map((p) => p.key)
);

/** A fresh config object populated with schema defaults. */
export function createDefaultConfig() {
  const config = {};
  for (const [key, param] of PARAMS) config[key] = param.value;
  return config;
}

