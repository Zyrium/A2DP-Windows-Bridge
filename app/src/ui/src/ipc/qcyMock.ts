// A fake QCY MeloBuds N70 for the Fones screen in a plain browser and in
// the tests: every change is "confirmed" after a short delay, LDAC
// changes take longer (the headset reconnects the audio). Nothing here
// talks to Bluetooth.

import { EQ_PRESETS } from "../lib/eqPresets";
import type { QcyChange, QcyProvider, QcyState } from "./qcy";

export interface MockQcyOptions {
  /** Start with the BLE control link failed (the yellow banner) */
  unavailable?: boolean;
  /** Like the real build of this version: values only, set() refused */
  readOnly?: boolean;
  /** Start with this unavailable reason instead of gatt_failed */
  reason?: "le_off" | "not_found" | "gatt_failed";
  /** Like this version: only game mode can change, with the write flow;
   *  "fail" never confirms (the 3 s timeout) */
  gameOnly?: boolean | "fail";
  /** Multiplies every delay (0 in tests) */
  speed?: number;
}

export function sampleQcyState(): QcyState {
  const now = Date.now();
  return {
    link: { state: "connected" },
    device_name: "QCY MeloBuds N70",
    address: "F4:4E:FD:00:12:70",
    firmware: "3.0.13",
    battery: {
      left: { level: 54, charging: false },
      right: { level: 57, charging: false },
      case: { level: 80, charging: true },
    },
    noise_mode: "anc",
    noise_scene: "adaptive",
    game_mode: false,
    sleep_mode: false,
    wind_reduction: true,
    wear_detection: true,
    ldac: true,
    multipoint: false,
    prompt_volume: 9,
    prompt_volume_max: 15,
    auto_off: 30,
    eq: { preset: "spatial", gains: [...EQ_PRESETS[1].gains], master_gain: 0 },
    gestures: {
      left: { double_tap: "play_pause", long_press: "noise_control" },
      right: { double_tap: "next", long_press: "voice_assistant" },
    },
    supported_gestures: ["double_tap", "long_press"],
    reapply_on_connect: true,
    profile_saved_at: now - 2 * 86400_000,
    synced_at: now - 4000,
  };
}

export class MockQcy implements QcyProvider {
  private s: QcyState;
  private subs = new Set<(s: QcyState) => void>();
  private speed: number;
  private readOnly: boolean;
  private gameOnly: boolean | "fail";
  /** Changes received, for the tests */
  readonly calls: QcyChange[] = [];

  constructor(o: MockQcyOptions = {}) {
    this.speed = o.speed ?? 1;
    this.s = sampleQcyState();
    this.readOnly = o.readOnly === true;
    if (this.readOnly) this.s.read_only = true;
    this.gameOnly = o.gameOnly ?? false;
    if (this.gameOnly) {
      this.s.read_only = false;
      this.s.writable = ["game_mode"];
      this.s.verified = ["game_mode"];
      this.s.write = null;
    }
    if (o.unavailable || o.reason) {
      const reason = o.reason ?? "gatt_failed";
      this.s.link = reason === "gatt_failed" ? { state: "unavailable", reason, detail: "GATT: 0x85" } : { state: "unavailable", reason };
      this.s.synced_at = Date.now() - 12 * 60_000;
    }
  }

  private wait(ms: number) {
    return new Promise<void>((r) => setTimeout(r, ms * this.speed));
  }
  private push() {
    const copy = structuredClone(this.s);
    for (const f of this.subs) f(copy);
    return copy;
  }
  private requireLink() {
    if (this.s.link.state !== "connected") {
      throw { code: "qcy_unavailable", msg: { key: "fones.unavailable.title", args: [] } };
    }
  }

  async state() {
    return structuredClone(this.s);
  }

  async set(c: QcyChange) {
    this.calls.push(c);
    if (this.readOnly) throw { code: "qcy.read_only", msg: { key: "fones.error.read_only", args: [] } };
    if (this.gameOnly) return this.setGame(c);
    this.requireLink();
    await this.wait(c.kind === "ldac" ? 2500 : 250);
    switch (c.kind) {
      case "noise":
        this.s.noise_mode = c.mode;
        if (c.scene) this.s.noise_scene = c.scene;
        break;
      case "prompt_volume":
        this.s.prompt_volume = Math.max(0, Math.min(this.s.prompt_volume_max, Math.round(c.value)));
        break;
      case "auto_off":
        this.s.auto_off = c.minutes;
        break;
      case "eq":
        this.s.eq = structuredClone(c.eq);
        break;
      case "gesture":
        if (!this.s.supported_gestures.includes(c.gesture)) {
          throw { code: "qcy_unsupported", msg: { key: "fones.gestures.unsupported", args: [] } };
        }
        this.s.gestures[c.side][c.gesture] = c.action;
        break;
      default:
        this.s[c.kind] = c.enabled;
    }
    this.s.synced_at = Date.now();
    return this.push();
  }

  /** The write flow of the core (qcy_protocol::write), faked */
  private async setGame(c: QcyChange) {
    if (c.kind !== "game_mode") throw { code: "qcy.read_only", msg: { key: "fones.error.read_only", args: [] } };
    this.requireLink();
    const previous = this.s.game_mode;
    const sent = c.enabled ? "FF03090101" : "FF03090102";
    const steps = [
      { what: "read_previous", detail: `FF03FE0109 -> ${previous ? "on" : "off"}` },
      { what: "sent", detail: sent },
    ];
    await this.wait(this.gameOnly === "fail" ? 3000 : 400);
    if (this.gameOnly === "fail") {
      steps.push({ what: "no_read_back", detail: "3000 ms" });
      this.s.write = { setting: "game_mode", enabled: c.enabled, previous, status: "failed", failure: "timeout", sent, read_back: null, steps, at_ms: Date.now() };
    } else {
      const read_back = c.enabled ? "090101" : "090102";
      steps.push({ what: "read_back", detail: read_back });
      this.s.game_mode = c.enabled;
      this.s.synced_at = Date.now();
      this.s.write = { setting: "game_mode", enabled: c.enabled, previous, status: "confirmed", failure: null, sent, read_back, steps, at_ms: Date.now() };
    }
    return this.push();
  }

  async refresh() {
    this.requireLink();
    await this.wait(400);
    this.s.synced_at = Date.now();
    return this.push();
  }

  async reconnect() {
    this.s.link = { state: "connecting" };
    this.push();
    await this.wait(1200);
    this.s.link = { state: "connected" };
    this.s.synced_at = Date.now();
    return this.push();
  }

  async saveProfile() {
    this.s.profile_saved_at = Date.now();
    return this.push();
  }

  onState(cb: (s: QcyState) => void) {
    this.subs.add(cb);
    return () => void this.subs.delete(cb);
  }
}
