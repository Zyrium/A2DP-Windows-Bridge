// The EQ presets of the Fones screen (our own curves, see the phase 4
// notes): ten bands, EQ_BANDS_HZ order, dB. The headset only stores the
// gains; the preset is the app's name for them.

export interface EqPreset {
  id: string;
  /** Translation key suffix: fones.eq.preset.<id> */
  gains: number[];
  master_gain: number;
}

export const EQ_PRESETS: EqPreset[] = [
  { id: "default", gains: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0], master_gain: 0 },
  { id: "spatial", gains: [1, 1, 0, -1, -1.5, 0, 1.5, 2.5, 3, 3], master_gain: 0 },
  { id: "soft_stage", gains: [0, 0.5, 1, 0.5, 0, -0.5, -1, -0.5, 0.5, 1], master_gain: 0 },
  { id: "pop", gains: [-1, 0, 1.5, 2.5, 2, 1, 0, 1, 1.5, 1], master_gain: 0 },
  // The bass boost reaches +6 dB: -3 dB overall keeps it from clipping
  { id: "bass", gains: [6, 5.5, 4, 2, 0.5, 0, 0, 0, 0, 0], master_gain: -3 },
  { id: "rock", gains: [4, 3, 1.5, -0.5, -1.5, -1, 1, 2.5, 3.5, 3.5], master_gain: 0 },
  { id: "mellow", gains: [1, 1, 0.5, 0, 0, -1, -2, -2.5, -2, -1.5], master_gain: 0 },
  { id: "classical", gains: [2.5, 2, 1, 0, -0.5, -0.5, 0, 1, 2, 2.5], master_gain: 0 },
];

/** The preset whose gains these are, else "custom" */
export function matchPreset(gains: number[], master: number): string {
  const p = EQ_PRESETS.find((p) => p.master_gain === master && p.gains.every((g, i) => g === gains[i]));
  return p ? p.id : "custom";
}
