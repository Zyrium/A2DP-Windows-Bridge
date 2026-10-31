// The contract between the "Fones" screen and the QCY headset control
// (BLE/GATT, crates/qcy-protocol). The Rust side: src-tauri/commands.rs
// (qcy_*) and the core's qcy.* requests (core/src/qcy_live.rs). This
// version reads everything and changes ONE setting: game mode (`writable`,
// still "testing"). Any other qcy_set and qcy_save_profile reject with
// qcy.read_only. In a plain browser, or with ?qcy=mock, the mock provider of
// qcyMock.ts stands in for it.
//
// Tauri commands (invoke):
//   qcy_state()                      -> QcyState      the last known state
//   qcy_set({ change: QcyChange })   -> QcyState      one change, sent to the headset
//   qcy_refresh()                    -> QcyState      reads everything again
//   qcy_reconnect()                  -> QcyState      retries the BLE control link
//   qcy_save_profile()               -> QcyState      stores the current values
// Tauri events (listen):
//   qcy://state   payload QcyState   after every change, read or link change
//
// Errors reject with an ErrorInfo-shaped value, as the core's requests do.

/** The BLE control link (the audio, A2DP, is separate and unaffected) */
export type QcyLink =
  | { state: "connected" }
  | { state: "connecting" }
  | { state: "unavailable"; reason: QcyUnavailableReason; detail?: string }
  /** No QCY headset in the active profile */
  | { state: "none" };

export type QcyUnavailableReason = "not_found" | "gatt_failed" | "timeout" | "adapter_busy" | "not_implemented" | "le_off" | "other";

export type NoiseMode = "off" | "anc" | "transparency";
export type NoiseScene = "adaptive" | "indoor" | "commuting" | "noisy" | "anti_wind";

/** The touch actions of the protocol, in its order (0..8) */
export const TOUCH_ACTIONS = [
  "none",
  "play_pause",
  "previous",
  "next",
  "voice_assistant",
  "volume_up",
  "volume_down",
  "game_mode",
  "noise_control",
] as const;
export type TouchAction = (typeof TOUCH_ACTIONS)[number];

export type GestureKind = "single_tap" | "double_tap" | "triple_tap" | "long_press";
export type Side = "left" | "right";
/** Protocol key ids: double tap 0x03/0x04, long press 0x09/0x0A (left/right) */
export const GESTURE_KEYS: Partial<Record<GestureKind, Record<Side, number>>> = {
  double_tap: { left: 0x03, right: 0x04 },
  long_press: { left: 0x09, right: 0x0a },
};

export interface Battery {
  /** 0..100, null when not reported (bud in the case, case closed) */
  level: number | null;
  charging: boolean;
}

export const EQ_BANDS_HZ = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000] as const;
export const EQ_GAIN_MIN = -8;
export const EQ_GAIN_MAX = 8;

export interface Equalizer {
  /** A preset id of eqPresets.ts, or "custom" */
  preset: string;
  /** Ten gains in dB, EQ_BANDS_HZ order, -8..+8 */
  gains: number[];
  /** dB, applied to every band (headroom for bass-heavy curves) */
  master_gain: number;
}

export type Gestures = Record<Side, Partial<Record<GestureKind, TouchAction>>>;

/** auto power off: minutes, null = never (0xFFFF on the wire) */
export type AutoOff = null | 5 | 15 | 30 | 60;

export interface QcyState {
  link: QcyLink;
  /** The values are read from the headset but cannot be changed (this
   *  version): the controls stay greyed, "Atualizar" reads again */
  read_only?: boolean;
  /** The only settings that can change (this version: ["game_mode"]);
   *  absent: every control works (the mock's full mode) */
  writable?: string[];
  /** Writable settings already checked on real earbuds (none yet: the
   *  others carry the "Em teste" tag) */
  verified?: string[];
  /** The last write, null before the first */
  write?: QcyWrite | null;
  /** The headset's own equalizer preset number, when it reported one */
  eq_preset_index?: number | null;
  device_name: string;
  address: string;
  firmware: string | null;
  battery: { left: Battery; right: Battery; case: Battery };
  noise_mode: NoiseMode;
  noise_scene: NoiseScene;
  game_mode: boolean;
  sleep_mode: boolean;
  wind_reduction: boolean;
  wear_detection: boolean;
  ldac: boolean;
  multipoint: boolean;
  prompt_volume: number;
  prompt_volume_max: number;
  auto_off: AutoOff;
  eq: Equalizer;
  gestures: Gestures;
  /** The gestures the headset accepts changes for */
  supported_gestures: GestureKind[];
  reapply_on_connect: boolean;
  /** Unix ms of the stored profile, null when none */
  profile_saved_at: number | null;
  /** Unix ms of the last read from the headset, null before the first */
  synced_at: number | null;
}

/** One step of a write, as the core logged it */
export interface QcyWriteStep {
  what: "read_previous" | "read_previous_failed" | "sent" | "send_failed" | "read_back" | "no_read_back" | "refused" | "no_link" | string;
  detail: string;
}

/** A write: read the old value, send one command, read it back. Never
 *  re-sent: "failed" leaves the switch at the value read. */
export interface QcyWrite {
  setting: string;
  enabled: boolean;
  /** The value read before the write (what "Desfazer" goes back to) */
  previous: boolean | null;
  status: "confirmed" | "failed";
  failure: "no_previous" | "send_failed" | "timeout" | "mismatch" | QcyUnavailableReason | null;
  /** Hex of the frame sent (e.g. FF03090101), null when nothing was sent */
  sent: string | null;
  read_back: string | null;
  steps: QcyWriteStep[];
  at_ms: number;
}

/** One change. The values stay as they are until the headset confirms. */
export type QcyChange =
  | { kind: "noise"; mode: NoiseMode; scene?: NoiseScene }
  | { kind: "game_mode" | "sleep_mode" | "wind_reduction" | "wear_detection" | "ldac" | "multipoint" | "reapply_on_connect"; enabled: boolean }
  | { kind: "prompt_volume"; value: number }
  | { kind: "auto_off"; minutes: AutoOff }
  | { kind: "eq"; eq: Equalizer }
  | { kind: "gesture"; side: Side; gesture: GestureKind; action: TouchAction };

export const QCY_COMMANDS = {
  state: "qcy_state",
  set: "qcy_set",
  refresh: "qcy_refresh",
  reconnect: "qcy_reconnect",
  saveProfile: "qcy_save_profile",
} as const;
export const QCY_EVENT_STATE = "qcy://state";

export interface QcyProvider {
  state(): Promise<QcyState>;
  set(change: QcyChange): Promise<QcyState>;
  refresh(): Promise<QcyState>;
  reconnect(): Promise<QcyState>;
  saveProfile(): Promise<QcyState>;
  /** Every state push from now on; returns the unsubscribe */
  onState(cb: (s: QcyState) => void): () => void;
}

/** What the real build shows while the core has no qcy_* commands: no
 *  headset values (nothing is made up), link unavailable/not_implemented. */
export function placeholderQcyState(): QcyState {
  const none = { level: null, charging: false };
  return {
    link: { state: "unavailable", reason: "not_implemented" },
    device_name: "",
    address: "",
    firmware: null,
    battery: { left: { ...none }, right: { ...none }, case: { ...none } },
    noise_mode: "off",
    noise_scene: "adaptive",
    game_mode: false,
    sleep_mode: false,
    wind_reduction: false,
    wear_detection: false,
    ldac: false,
    multipoint: false,
    prompt_volume: 0,
    prompt_volume_max: 15,
    auto_off: null,
    eq: { preset: "custom", gains: EQ_BANDS_HZ.map(() => 0), master_gain: 0 },
    gestures: { left: {}, right: {} },
    supported_gestures: [],
    reapply_on_connect: false,
    profile_saved_at: null,
    synced_at: null,
  };
}

export async function createTauriQcy(): Promise<QcyProvider> {
  const { invoke: raw } = await import("@tauri-apps/api/core");
  // A core without the qcy_* commands (not written yet) rejects every
  // call: show the placeholder (the yellow banner) instead of failing.
  const invoke = <T,>(cmd: string, args?: Record<string, unknown>) =>
    raw<T>(cmd, args).catch((e) => {
      if (cmd === QCY_COMMANDS.state) return placeholderQcyState() as T;
      throw e;
    });
  const { listen } = await import("@tauri-apps/api/event");
  return {
    state: () => invoke<QcyState>(QCY_COMMANDS.state),
    set: (change) => invoke<QcyState>(QCY_COMMANDS.set, { change }),
    refresh: () => invoke<QcyState>(QCY_COMMANDS.refresh),
    reconnect: () => invoke<QcyState>(QCY_COMMANDS.reconnect),
    saveProfile: () => invoke<QcyState>(QCY_COMMANDS.saveProfile),
    onState(cb) {
      const un = listen<QcyState>(QCY_EVENT_STATE, (e) => cb(e.payload));
      return () => void un.then((f) => f());
    },
  };
}

/** The dev flag: ?qcy=mock (or ?qcy=unavailable) in the URL, or any plain
 *  browser (pnpm dev). Inside a2dpwb.exe without the flag: the real one. */
export async function createQcy(insideTauri: boolean): Promise<QcyProvider> {
  const flag = typeof location !== "undefined" ? new URLSearchParams(location.search).get("qcy") : null;
  if (flag === "placeholder") {
    // the real build without qcy_* commands, seen in a browser
    const rej = () => Promise.reject({ code: "qcy_unavailable", msg: { key: "fones.unavailable.not_implemented", args: [] } });
    return { state: async () => placeholderQcyState(), set: rej, refresh: rej, reconnect: rej, saveProfile: rej, onState: () => () => {} };
  }
  if (flag || !insideTauri || import.meta.env.VITE_QCY_MOCK === "1") {
    const { MockQcy } = await import("./qcyMock");
    // ?qcy=readonly, readonly-le_off, readonly-not_found: like this version
    const ro = flag?.startsWith("readonly") === true;
    const reason = flag?.split("-")[1] as "le_off" | "not_found" | undefined;
    // ?qcy=game (game mode writes confirm) or ?qcy=game-fail (3 s timeout)
    const gameOnly = flag === "game" ? true : flag === "game-fail" ? "fail" : false;
    return new MockQcy({ unavailable: flag === "unavailable", readOnly: ro, reason: gameOnly ? undefined : reason, gameOnly });
  }
  return createTauriQcy();
}
