// Device names on up to two lines, the full name as the tooltip only while
// it is cut

import { cleanup, render, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../app/App";
import { MockCore } from "../ipc/mock";
import { createCore } from "../ipc/store";
import { titleWhenCut } from "./controls";

let mock: MockCore | null = null;

afterEach(() => {
  cleanup();
  mock?.dispose();
  mock = null;
});

/** jsdom lays nothing out: the sizes a browser would give the element */
function sized(el: HTMLElement, size: { client: number; scroll: number }) {
  Object.defineProperty(el, "clientHeight", { configurable: true, get: () => size.client });
  Object.defineProperty(el, "scrollHeight", { configurable: true, get: () => size.scroll });
}

describe("titleWhenCut", () => {
  it("gives the full text as the tooltip only while it does not fit", async () => {
    const [name, setName] = createSignal("Earbuds Pro");
    // Two lines of 26 px shown; the text needs one, then three
    const size = { client: 53, scroll: 54 };
    let el!: HTMLDivElement;
    render(() => (
      <div
        ref={(e) => {
          el = e;
          sized(e, size);
          titleWhenCut(name)(e);
        }}
      >
        {name()}
      </div>
    ));
    await waitFor(() => expect(el.textContent).toBe("Earbuds Pro"));
    expect(el.hasAttribute("title")).toBe(false);
    size.scroll = 80;
    setName("Earbuds Pro with a name far too long for two lines");
    await waitFor(() => expect(el.title).toBe("Earbuds Pro with a name far too long for two lines"));
    // Fits again: no tooltip repeating what is shown
    size.scroll = 27;
    setName("Buds");
    await waitFor(() => expect(el.hasAttribute("title")).toBe(false));
  });
});

describe("device names", () => {
  it("take up to two lines on Home, in the sidebar and on Codecs", async () => {
    location.hash = "#/home";
    mock = new MockCore({ scenario: "normal", speed: 0 });
    render(() => <App core={createCore(mock!)} />);
    await waitFor(() => expect(document.querySelectorAll(".home-device .device-name")).toHaveLength(2));
    for (const el of document.querySelectorAll(".home-device .device-name, .status-pill-device")) {
      expect(el).toHaveClass("clamp-2");
      expect(el).not.toHaveClass("ellipsis");
    }
    // Nothing cut (jsdom): no tooltip on the card's name
    expect(document.querySelector(".home-device .device-name")!.hasAttribute("title")).toBe(false);
    location.hash = "#/codecs";
    await waitFor(() => expect(document.querySelectorAll(".device-codecs-title h2")).toHaveLength(2));
    for (const el of document.querySelectorAll(".device-codecs-title h2")) expect(el).toHaveClass("clamp-2");
  });
});
