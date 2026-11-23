import { describe, expect, it } from "vitest";
import { layoutOf, pairBigs } from "./Auracast";

describe("Auracast layout", () => {
  it("names the layout as the core takes it", () => {
    const c = { front: "stereo", rear: "separate" } as const;
    expect(layoutOf({ ...c, channels: "stereo" })).toBe("stereo");
    expect(layoutOf({ ...c, channels: "mono" })).toBeNull();
    expect(layoutOf({ ...c, channels: "split" })).toBe("split");
    expect(layoutOf({ ...c, channels: "matrix" })).toBe("matrix_stereo_separate");
  });

  it("counts the BIGs of each pair mode", () => {
    expect([pairBigs("stereo"), pairBigs("separate"), pairBigs("subgroups"), pairBigs("off")]).toEqual([1, 2, 1, 0]);
  });
});
