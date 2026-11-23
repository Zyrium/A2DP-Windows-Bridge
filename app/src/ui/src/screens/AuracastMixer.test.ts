import { describe, expect, it } from "vitest";
import { delayFrom, delayIn, gainText } from "./AuracastMixer";

describe("Auracast mixer fields", () => {
  it("shows delays in ms or metres and reads them back in ms", () => {
    expect(delayIn(1.234, "ms")).toBe("1.23");
    expect(delayIn(10, "m")).toBe("3.43");
    expect(delayFrom("3.43", "m")).toBe(10);
    expect(delayFrom("0,25", "ms")).toBe(0.25);
    expect(delayFrom("900", "ms")).toBe(500);
    expect(delayFrom("-1", "ms")).toBe(0);
    expect(delayFrom("", "ms")).toBeNull();
    expect(delayFrom("x", "m")).toBeNull();
  });

  it("shows gains without trailing zeros", () => {
    expect(gainText(0)).toBe("0");
    expect(gainText(1)).toBe("1");
    expect(gainText(0.70794)).toBe("0.708");
    expect(gainText(-0.5)).toBe("-0.5");
  });
});
