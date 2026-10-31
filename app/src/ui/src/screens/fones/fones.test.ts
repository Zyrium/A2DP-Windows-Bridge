import { describe, expect, it } from "vitest";
import { parseRoute, routeHash } from "../../app/context";
import { MockQcy } from "../../ipc/qcyMock";
import { EQ_BANDS_HZ } from "../../ipc/qcy";
import { EQ_PRESETS, matchPreset } from "../../lib/eqPresets";
import { canRefresh, curvePath, writeStepLines } from "./Fones";

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
  it("game mode only: reads, one command, read-back; the rest is refused", async () => {
    const q = new MockQcy({ speed: 0, gameOnly: true });
    expect((await q.state()).writable).toEqual(["game_mode"]);
    const on = await q.set({ kind: "game_mode", enabled: true });
    expect(on.game_mode).toBe(true);
    expect(on.write).toMatchObject({ status: "confirmed", previous: false, sent: "FF03090101", failure: null });
    expect(on.write!.steps.map((s) => s.what)).toEqual(["read_previous", "sent", "read_back"]);
    expect(writeStepLines(on.write!)).toHaveLength(3);
    // Desfazer: the same flow back to the saved value
    const undo = await q.set({ kind: "game_mode", enabled: on.write!.previous! });
    expect(undo.game_mode).toBe(false);
    expect(undo.write).toMatchObject({ status: "confirmed", previous: true, sent: "FF03090102" });
    for (const c of [{ kind: "sleep_mode", enabled: true }, { kind: "ldac", enabled: false }, { kind: "noise", mode: "off" }] as const) {
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