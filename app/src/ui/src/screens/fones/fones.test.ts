import { describe, expect, it } from "vitest";
import { parseRoute, routeHash } from "../../app/context";
import { MockQcy } from "../../ipc/qcyMock";
import { EQ_BANDS_HZ, NOISE_CODES, noiseCode } from "../../ipc/qcy";
import { EQ_PRESETS, matchPreset } from "../../lib/eqPresets";
import { wearBytes, canRefresh, curvePath, F2_SETTINGS, needsNoiseWarning, noiseChange, noiseText, promptMax, settingName, valueText, windAllowed, writeChange, writeStepLines } from "./Fones";
import { ft } from "./strings";
import { setLanguage } from "../../i18n";
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
    expect((await q.state()).writable).toEqual(["game_mode", "sleep_mode", "wind_reduction", "noise_control", "wear_detection"]);
    expect((await q.state()).experimental).toEqual([]);
    expect((await q.state()).verified).toEqual(["game_mode", "sleep_mode", "wind_reduction", "noise_control", "wear_detection"]);
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

describe("phase 2 (sleep, wind; prompt volume read-only)", () => {
  it("allowlist is exactly 0x09, 0x10, 0x2A (+ 0x17 since phase 3)", () => {
    expect(F2_OPCODES).toEqual({ game_mode: 0x09, sleep_mode: 0x10, wind_reduction: 0x2a, noise_control: 0x17, wear_detection: 0x2c });
    expect(F2_SETTINGS).toEqual(["game_mode", "sleep_mode", "wind_reduction"]);
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

  it("prompt volume is read-only: refused, nothing sent", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    await expect(q.set({ kind: "prompt_volume", value: 10 })).rejects.toMatchObject({ code: "qcy.read_only" });
    const s = await q.state();
    expect(s.prompt_volume).toBe(9);
    expect(s.write).toBeNull();
  });

  it("timeout: value kept, nothing re-sent, for every new setting", async () => {
    for (const c of [{ kind: "sleep_mode", enabled: true }, { kind: "wind_reduction", enabled: false }] as const) {
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
    expect(windAllowed({ ...s, noise_mode: null })).toBe(true);
  });

  it("prompt volume shows the reported level (fallback max 15)", () => {
    const s = sampleQcyState();
    expect(promptMax({ ...s, prompt_volume_max: 0 })).toBe(15);
    expect(promptMax({ ...s, prompt_volume_max: 15 })).toBe(15);
    expect(settingName("prompt_volume")).not.toBe("prompt_volume");
  });
});


describe("phase 3 (noise control 0x17)", () => {
  it("exactly the seven states of the mockup", () => {
    expect(NOISE_CODES.map((n) => [n.code, n.bytes])).toEqual([
      [0, "020000"],
      [1, "010500"],
      [2, "010102"],
      [3, "010202"],
      [4, "010302"],
      [5, "010400"],
      [6, "030104"],
    ]);
    expect(noiseCode("off")).toBe(0);
    expect(noiseCode("transparency", "noisy")).toBe(6);
    expect(noiseCode("anc", "anti_wind")).toBe(5);
    expect(noiseCode("anc")).toBe(1);
  });

  it("sends a code in write mode, the old change in the full mock", async () => {
    const w = await new MockQcy({ speed: 0, gameOnly: true }).state();
    expect(w.writable).toContain("noise_control");
    expect(noiseChange(w, "anc", "indoor")).toEqual({ kind: "noise_control", value: 2 });
    expect(noiseChange(w, "transparency")).toEqual({ kind: "noise_control", value: 6 });
    // ANC with no scene picked keeps the scene read
    expect(noiseChange({ ...w, noise_scene: "noisy" }, "anc")).toEqual({ kind: "noise_control", value: 4 });
    const full = await new MockQcy({ speed: 0 }).state();
    expect(noiseChange(full, "off")).toEqual({ kind: "noise", mode: "off" });
  });

  it("asks before Desligado or Transparência, not for the ANC scenes", async () => {
    const w = await new MockQcy({ speed: 0, gameOnly: true }).state();
    expect(w.noise_mode).toBe("anc");
    expect(needsNoiseWarning(w, "transparency")).toBe(true);
    expect(needsNoiseWarning(w, "off")).toBe(true);
    expect(needsNoiseWarning(w, "anc")).toBe(false);
    expect(needsNoiseWarning({ ...w, noise_mode: "transparency" }, "transparency")).toBe(false);
    expect(needsNoiseWarning(await new MockQcy({ speed: 0 }).state(), "off")).toBe(false);
  });

  it("transparency confirmed: 3 bytes read back, wind read again, Desfazer", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    const s = await q.set({ kind: "noise_control", value: 6 });
    expect(s.noise_mode).toBe("transparency");
    expect(s.wind_reduction).toBe(false);
    expect(s.write).toMatchObject({ setting: "noise_control", status: "confirmed", value: 6, previous: 1, sent: "FF051703030104", read_back: "030104" });
    expect(s.write!.steps.map((x) => x.what)).toEqual(["read_previous", "sent", "read_back", "read_wind"]);
    expect(s.write!.steps.filter((x) => x.what === "sent")).toHaveLength(1);
    expect(writeChange(s.write!, "undo")).toEqual({ kind: "noise_control", value: 1 });
    expect(writeChange(s.write!, "retry")).toEqual({ kind: "noise_control", value: 6 });
    expect(writeStepLines(s.write!)).toHaveLength(4);
    expect(noiseText(5)).not.toBe("?");
    expect(noiseText(9)).toBe("?");
    expect(settingName("noise_control")).not.toBe("noise_control");
    // No wind write is ever sent with it
    expect(q.calls).toEqual([{ kind: "noise_control", value: 6 }]);
  });

  it("timeout: the mode read stays, nothing re-sent, wind untouched", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: "fail" });
    const s = await q.set({ kind: "noise_control", value: 0 });
    expect(s.write).toMatchObject({ status: "failed", failure: "timeout", read_back: null, previous: 1 });
    expect(s.write!.steps.filter((x) => x.what === "sent")).toHaveLength(1);
    expect(s.write!.steps.some((x) => x.what === "read_wind")).toBe(false);
    expect(s.noise_mode).toBe("anc");
    expect(s.noise_scene).toBe("adaptive");
    expect(s.wind_reduction).toBe(true);
    expect(q.calls).toHaveLength(1);
  });

  it("refuses codes outside the seven", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    await expect(q.set({ kind: "noise_control", value: 7 })).rejects.toMatchObject({ code: "qcy.read_only" });
    expect((await q.state()).write).toBeNull();
    expect(writeChange({ setting: "noise_control", enabled: null, value: 1, previous: 9, status: "confirmed", failure: null, sent: null, read_back: null, steps: [], at_ms: 0 }, "undo")).toBeNull();
  });
});

describe("phase 3b (wear detection 0x2C, Verificado)", () => {
  it("off: one frame copying music/ANC, 00 read back counts as off, Desfazer", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    const off = await q.set({ kind: "wear_detection", enabled: false });
    expect(off.wear_detection).toBe(false);
    expect(off.write).toMatchObject({ setting: "wear_detection", status: "confirmed", previous: true, sent: "FF052C03020101", read_back: "000101" });
    expect(off.write!.steps.filter((x) => x.what === "sent")).toHaveLength(1);
    const lines = writeStepLines(off.write!);
    expect(lines).toHaveLength(3);
    expect(lines[2]).toContain("000101");
    expect(wearBytes(off.wear_read!)).toBe("00 01 01");
    expect(settingName("wear_detection")).not.toBe("wear_detection");
    expect(writeChange(off.write!, "undo")).toEqual({ kind: "wear_detection", enabled: true });
    const on = await q.set(writeChange(off.write!, "undo")!);
    expect(on.write).toMatchObject({ status: "confirmed", sent: "FF052C03010101", read_back: "010101" });
  });

  it("pt-BR texts are feminine (a Detecção de uso)", async () => {
    await setLanguage("pt-BR");
    const q = new MockQcy({ speed: 0, gameOnly: true });
    const off = await q.set({ kind: "wear_detection", enabled: false });
    expect(valueText(off.write!, true)).toBe("ligada");
    expect(valueText(off.write!, false)).toBe("desligada");
    expect(writeStepLines(off.write!)[0]).toBe("Valor anterior lido e salvo: ligada");
    expect(ft("write.undo", valueText(off.write!, off.write!.previous))).toBe("↶ Desfazer (voltar para ligada)");
    const on = await q.set(writeChange(off.write!, "undo")!);
    expect(writeStepLines(on.write!)[0]).toBe("Valor anterior lido e salvo: desligada");
    expect(writeStepLines(on.write!)[2]).toBe("Confirmado pelo fone: ligada ✓");
    expect(ft("write.ok_on_f", ft("wear"))).toBe("Detecção de uso ligada — confirmada pelo fone");
    expect(ft("write.ok_off_f", ft("wear"))).toBe("Detecção de uso desligada — confirmada pelo fone");
    // the other settings keep the masculine forms
    const g = await q.set({ kind: "game_mode", enabled: true });
    expect(valueText(g.write!, false)).toBe("desligado");
    await setLanguage("en");
  });

  it("keeps a tone byte and times out once without re-sending", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    (q as unknown as { s: { wear_read: unknown } }).s.wear_read = { enabled: true, music: 1, anc: 1, tone: true };
    const t = await q.set({ kind: "wear_detection", enabled: false });
    expect(t.write!.sent).toBe("FF062C0402010101");
    const f = new MockQcy({ speed: 0, gameOnly: "fail" });
    const s = await f.set({ kind: "wear_detection", enabled: false });
    expect(s.wear_detection).toBe(true);
    expect(s.write).toMatchObject({ status: "failed", failure: "timeout", read_back: null });
    expect(f.calls).toHaveLength(1);
  });

  it("never writes on open", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    await q.state();
    expect(q.calls).toHaveLength(0);
    expect((await q.state()).write).toBeNull();
  });
});
