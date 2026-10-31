import { describe, expect, it } from "vitest";
import { parseRoute, routeHash } from "../../app/context";
import { MockQcy } from "../../ipc/qcyMock";
import { EQ_BANDS_HZ } from "../../ipc/qcy";
import { EQ_PRESETS, matchPreset } from "../../lib/eqPresets";
import { canRefresh, curvePath, promptMax, settingName, stepperNext, windAllowed, writeChange, writeStepLines } from "./Fones";
import { F2_OPCODES, sampleQcyState } from "../../ipc/qcyMock";

describe("fones", () => {
  it("routes the three views", () => {
    expect(parseRoute("#/fones")).toEqual({ name: "fones" });
    expect(parseRoute("#/fones/eq")).toEqual({ name: "fones", arg: "eq" });
    expect(routeHash({ name: "fones", arg: "settings" })).toBe("#/fones/settings");
  });

  it("has ten bands per preset and -3 dB on the bass boost", () => {
    for (const p of EQ_PRESETS) expect(p.gains).toHaveLength(EQ_BANDS_HZ.length);
    expect(EQ_PRESETS.find((p) => p.id === "bass")!.master_gain).toBe(-3);
    expect(matchPreset([6, 5.5, 4, 2, 0.5, 0, 0, 0, 0, 0], -3)).toBe("bass");
    expect(matchPreset([6, 5.5, 4, 2, 0.5, 0, 0, 0, 0, 0], 0)).toBe("custom");
    expect(curvePath([0, 0])).toBe("M0.0 9.0 L56.0 9.0");
  });

  it("mock confirms changes and refuses them while unavailable", async () => {
    const q = new MockQcy({ speed: 0 });
    const pushed: boolean[] = [];
    q.onState((s) => pushed.push(s.ldac));
    expect((await q.set({ kind: "ldac", enabled: false })).ldac).toBe(false);
    expect(pushed).toEqual([false]);
    await expect(q.set({ kind: "gesture", side: "left", gesture: "single_tap", action: "next" })).rejects.toMatchObject({ code: "qcy_unsupported" });
    const off = new MockQcy({ speed: 0, unavailable: true });
    await expect(off.set({ kind: "game_mode", enabled: true })).rejects.toMatchObject({ code: "qcy_unavailable" });
    expect((await off.reconnect()).link.state).toBe("connected");
  });
});

describe("read only (this version)", () => {
  it("shows the values and refuses every change", async () => {
    const q = new MockQcy({ speed: 0, readOnly: true });
    const s = await q.state();
    expect(s.read_only).toBe(true);
    expect(s.link.state).toBe("connected");
    await expect(q.set({ kind: "ldac", enabled: false })).rejects.toMatchObject({ code: "qcy.read_only" });
    expect((await q.state()).ldac).toBe(true);
    const le = new MockQcy({ speed: 0, readOnly: true, reason: "le_off" });
    expect((await le.state()).link).toEqual({ state: "unavailable", reason: "le_off" });
  });
});

import { placeholderQcyState } from "../../ipc/qcy";
describe("placeholder (real build, no qcy_* commands)", () => {
  it("phase 2: reads, one command, read-back; the rest is refused", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    expect((await q.state()).writable).toEqual(["game_mode", "sleep_mode", "wind_reduction", "prompt_volume"]);
    expect((await q.state()).verified).toEqual(["game_mode"]);
    const on = await q.set({ kind: "game_mode", enabled: true });
    expect(on.game_mode).toBe(true);
    expect(on.write).toMatchObject({ status: "confirmed", previous: false, sent: "FF03090101", failure: null });
    expect(on.write!.steps.map((s) => s.what)).toEqual(["read_previous", "sent", "read_back"]);
    expect(writeStepLines(on.write!)).toHaveLength(3);
    // Desfazer: the same flow back to the saved value
    const undo = await q.set({ kind: "game_mode", enabled: on.write!.previous as boolean });
    expect(undo.game_mode).toBe(false);
    expect(undo.write).toMatchObject({ status: "confirmed", previous: true, sent: "FF03090102" });
    for (const c of [
      { kind: "ldac", enabled: false },
      { kind: "noise", mode: "off" },
      { kind: "wear_detection", enabled: false },
      { kind: "multipoint", enabled: true },
      { kind: "reapply_on_connect", enabled: true },
      { kind: "auto_off", minutes: 5 },
    ] as const) {
      await expect(q.set(c)).rejects.toMatchObject({ code: "qcy.read_only" });
    }
  });

  it("game mode timeout: the switch keeps the value read, nothing re-sent", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: "fail" });
    const s = await q.set({ kind: "game_mode", enabled: true });
    expect(s.game_mode).toBe(false);
    expect(s.write).toMatchObject({ status: "failed", failure: "timeout", read_back: null });
    expect(s.write!.steps.filter((x) => x.what === "sent")).toHaveLength(1);
    expect(q.calls).toHaveLength(1);
  });

  it("shows no headset data and the not_implemented reason", () => {
    const s = placeholderQcyState();
    expect(s.link).toEqual({ state: "unavailable", reason: "not_implemented" });
    expect(s.device_name).toBe("");
    expect(s.battery.left.level).toBeNull();
    expect(s.synced_at).toBeNull();
  });
});

describe("Atualizar", () => {
  it("is enabled with link none in the game-mode build (read_only false)", async () => {
    const s = { ...(await new MockQcy({ speed: 0, gameOnly: true }).state()), read_only: false, writable: ["game_mode"], link: { state: "none" as const } };
    expect(canRefresh(s, false)).toBe(true);
    expect(canRefresh(s, true)).toBe(false);
    expect(canRefresh({ ...s, link: { state: "connecting" as const } }, false)).toBe(false);
    const off = await new MockQcy({ speed: 0, unavailable: true }).state();
    expect(off.link.state).toBe("unavailable");
    expect(canRefresh(off, false)).toBe(true);
  });
});

describe("phase 2 (sleep, wind, prompt volume)", () => {
  it("allowlist is exactly 0x09, 0x10, 0x2A, 0x1D", () => {
    expect(F2_OPCODES).toEqual({ game_mode: 0x09, sleep_mode: 0x10, wind_reduction: 0x2a, prompt_volume: 0x1d });
    for (const op of Object.values(F2_OPCODES)) expect([0x01, 0x02, 0x03]).not.toContain(op);
  });

  it("sleep and wind: one frame each, read back, Desfazer", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    const sl = await q.set({ kind: "sleep_mode", enabled: true });
    expect(sl.sleep_mode).toBe(true);
    expect(sl.write).toMatchObject({ setting: "sleep_mode", status: "confirmed", previous: false, sent: "FF03100101", read_back: "100101" });
    expect(writeChange(sl.write!, "undo")).toEqual({ kind: "sleep_mode", enabled: false });
    expect(writeStepLines(sl.write!)).toHaveLength(3);
    const wd = await q.set({ kind: "wind_reduction", enabled: false });
    expect(wd.write).toMatchObject({ setting: "wind_reduction", status: "confirmed", previous: true, sent: "FF032A0102", read_back: "2A0102" });
    expect(writeChange(wd.write!, "undo")).toEqual({ kind: "wind_reduction", enabled: true });
    expect(q.calls).toHaveLength(2);
  });

  it("prompt volume: one frame per step, exact read-back, Desfazer to the saved level", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    const s = await q.set({ kind: "prompt_volume", value: 10 });
    expect(s.prompt_volume).toBe(10);
    expect(s.write).toMatchObject({ setting: "prompt_volume", value: 10, previous: 9, max: 15, status: "confirmed", sent: "FF031D010A", read_back: "1D020A0F" });
    expect(writeChange(s.write!, "undo")).toEqual({ kind: "prompt_volume", value: 9 });
    expect(writeChange(s.write!, "retry")).toEqual({ kind: "prompt_volume", value: 10 });
    expect(writeStepLines(s.write!)[1]).toContain("10/15");
  });

  it("prompt volume above the reported max is not sent", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    const s = await q.set({ kind: "prompt_volume", value: 16 });
    expect(s.prompt_volume).toBe(9);
    expect(s.write).toMatchObject({ status: "failed", failure: "out_of_range", sent: null });
  });

  it("timeout: value kept, nothing re-sent, for every new setting", async () => {
    for (const c of [{ kind: "sleep_mode", enabled: true }, { kind: "wind_reduction", enabled: false }, { kind: "prompt_volume", value: 10 }] as const) {
      const q = new MockQcy({ speed: 0, gameOnly: "fail" });
      const s = await q.set(c);
      expect(s.write).toMatchObject({ status: "failed", failure: "timeout", read_back: null });
      expect(s.write!.steps.filter((x) => x.what === "sent")).toHaveLength(1);
      expect(s.sleep_mode).toBe(false);
      expect(s.wind_reduction).toBe(true);
      expect(s.prompt_volume).toBe(9);
      expect(q.calls).toHaveLength(1);
    }
  });

  it("wind reduction needs ANC", () => {
    const s = sampleQcyState();
    expect(windAllowed({ ...s, noise_mode: "anc" })).toBe(true);
    expect(windAllowed({ ...s, noise_mode: "off" })).toBe(false);
    expect(windAllowed({ ...s, noise_mode: "transparency" })).toBe(false);
  });

  it("stepper: one level per tap, clamped to 0..max (fallback 15)", () => {
    const s = sampleQcyState();
    expect(stepperNext({ ...s, prompt_volume: 9 }, 1)).toBe(10);
    expect(stepperNext({ ...s, prompt_volume: 9 }, -1)).toBe(8);
    expect(stepperNext({ ...s, prompt_volume: 15 }, 1)).toBeNull();
    expect(stepperNext({ ...s, prompt_volume: 0 }, -1)).toBeNull();
    expect(stepperNext({ ...s, prompt_volume: 4, prompt_volume_max: 4 }, 1)).toBeNull();
    expect(promptMax({ ...s, prompt_volume_max: 0 })).toBe(15);
    expect(stepperNext({ ...s, prompt_volume: 40, prompt_volume_max: 0 }, -1)).toBe(14);
    expect(settingName("prompt_volume")).not.toBe("prompt_volume");
  });
});

