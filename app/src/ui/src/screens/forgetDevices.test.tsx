// Removing devices on Audio Devices takes them off Home (cards and the
// sidebar) and Codecs at once, classic and LE Audio entries alike
// (they used to stay there for a while)

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../app/App";
import { setLanguage, t } from "../i18n";
import { MockCore, type MockScenario } from "../ipc/mock";
import { createCore } from "../ipc/store";
import { audioEntries } from "../lib/profile";

let mock: MockCore | null = null;

afterEach(async () => {
  cleanup();
  mock?.dispose();
  mock = null;
  await setLanguage("en");
});

const removeButtons = () => screen.queryAllByRole("button", { name: (n) => n.startsWith(`${t("ui.device.forget")}: `) });

describe("removing every device on Audio Devices", () => {
  // LE Audio off; on; on with an adapter no stack has checked yet (Home and
  // Codecs list its LE Audio entries, so Audio Devices does too)
  for (const scenario of ["normal", "le", "le_unchecked"] as MockScenario[]) {
    it(`leaves no device on Home, in the sidebar or on Codecs (${scenario})`, async () => {
      location.hash = "#/devices";
      mock = new MockCore({ scenario, speed: 0 });
      const core = createCore(mock);
      render(() => <App core={core} />);
      await waitFor(() => expect(core.state.snap).not.toBeNull());
      const listed = audioEntries(core.state.snap!).map((e) => e.name);
      expect(listed.length).toBeGreaterThan(0);
      // Each entry Home shows has its Remove button here
      await waitFor(() =>
        expect(removeButtons().map((b) => b.getAttribute("aria-label")!.slice(t("ui.device.forget").length + 2)).sort()).toEqual(
          [...listed].sort(),
        ),
      );
      while (removeButtons().length > 0) {
        const before = removeButtons().length;
        fireEvent.click(removeButtons()[0]);
        const dialog = await screen.findByRole("dialog");
        fireEvent.click(within(dialog).getByRole("button", { name: t("ui.device.forget") }));
        await waitFor(() => expect(removeButtons().length).toBe(before - 1));
        await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
      }
      // Nothing left anywhere, with no other request or refresh
      expect(audioEntries(core.state.snap!)).toEqual([]);
      expect(core.state.snap!.profiles).toEqual([]);
      expect(document.querySelectorAll(".status-pill-device")).toHaveLength(0);
      location.hash = "#/home";
      await screen.findByText(t("ui.home.no_device"));
      expect(document.querySelectorAll(".home-device")).toHaveLength(0);
      location.hash = "#/codecs";
      await screen.findByText(t("ui.home.no_device"));
      for (const name of listed) expect(screen.queryByRole("region", { name })).toBeNull();
    });
  }
});
