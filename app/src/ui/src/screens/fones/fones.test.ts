import { describe, expect, it } from "vitest";
import { parseRoute, routeHash } from "../../app/context";
import { MockQcy } from "../../ipc/qcyMock";
import { EQ_BANDS_HZ } from "../../ipc/qcy";
import { EQ_PRESETS, matchPreset } from "../../lib/eqPresets";
import { curvePath } from "./Fones";

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
  it("shows no headset data and the not_implemented reason", () => {
    const s = placeholderQcyState();
    expect(s.link).toEqual({ state: "unavailable", reason: "not_implemented" });
    expect(s.device_name).toBe("");
    expect(s.battery.left.level).toBeNull();
    expect(s.synced_at).toBeNull();
  });
});
