// Headset microphone (not only in developer mode): Off / Auto / On and the
// device under each device's Input Source on Audio Devices (issue #86; it
// was on Codecs), saved with the device's profile, the mute chip on Home's
// card and in the flyout (an icon, with the voice codec and why the
// microphone did not open in its tooltip), and the voice codec in the
// sidebar while a voice link runs

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../app/App";
import { setLanguage, t } from "../i18n";
import { MockCore, type MockScenario } from "../ipc/mock";
import type { Profile } from "../ipc/types";
import { createCore } from "../ipc/store";
import { endSettle } from "./Codecs";

let mock: MockCore | null = null;

function start(
  hash: string,
  scenario: MockScenario = "normal",
  window?: "flyout",
) {
  location.hash = hash;
  const calls: { method: string; params: unknown }[] = [];
  mock = new MockCore({
    scenario,
    speed: 0,
    ...(window ? { window } : {}),
    onCall: (method, params) => calls.push({ method, params }),
  });
  const core = createCore(mock);
  const view = render(() => <App core={core} />);
  const saves = () =>
    calls
      .filter((c) => c.method === "profile.save")
      .map((c) => (c.params as { profile: Profile }).profile);
  return { calls, core, view, saves };
}

afterEach(async () => {
  cleanup();
  endSettle();
  mock?.dispose();
  mock = null;
  await setLanguage("en");
});

const CABLE = "{0.0.0.00000000}.{cable}";
const VOICEMEETER = "{0.0.0.00000000}.{voicemeeter}";
/** A device's entry on Audio Devices */
const deviceEntry = (name: RegExp) =>
  waitFor(() => {
    const entry = [
      ...document.querySelectorAll<HTMLElement>("main .device-entry"),
    ].find((e) => name.test(e.textContent ?? ""));
    expect(entry).toBeTruthy();
    return entry!;
  });
const micModes = (entry: HTMLElement) =>
  within(entry).queryByRole("radiogroup", { name: t("capture.input_source") });
const mode = (entry: HTMLElement, m: "off" | "auto" | "on") =>
  within(micModes(entry)!).getByRole("radio", {
    name: t(`ui.headset_mic_${m}`),
  });
const micSelect = (entry: HTMLElement) =>
  within(entry).getByRole("combobox", {
    name: t("ui.mic_output"),
  }) as HTMLSelectElement;
const MUTE = () => t("ui.headset_mic_mute");
const UNMUTE = () => t("ui.headset_mic_unmute");

describe("Input Source on Audio Devices", () => {
  it("offers On only in developer mode; a device saved as On works as Auto meanwhile", async () => {
    const { core, saves } = start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(mode(wh, "on")).toBeChecked());
    const before = saves().length;
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: false },
    });
    // Off and Auto only, Auto chosen, nothing saved: On stays in the profile
    await waitFor(() => expect(mode(wh, "auto")).toBeChecked());
    expect(within(micModes(wh)!).getAllByRole("radio")).toHaveLength(2);
    expect(within(micModes(wh)!).queryByRole("radio", { name: t("ui.headset_mic_on") })).toBeNull();
    expect(saves().length).toBe(before);
    expect(core.state.snap!.profiles.find((p) => p.device_name === "WH-1000XM5")?.headset_mic).toBe("on");
    // Developer mode again: On is back
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    await waitFor(() => expect(mode(wh, "on")).toBeChecked());
    expect(within(micModes(wh)!).getAllByRole("radio")).toHaveLength(3);
  });

  it("is on every device outside developer mode too, and no longer on Codecs", async () => {
    const { core } = start("#/devices");
    const wh = await deviceEntry(/WH-1000XM5/);
    expect(core.state.snap?.settings.developer_mode).toBe(false);
    await waitFor(() => expect(mode(wh, "off")).toBeChecked());
    location.hash = "#/codecs";
    const card = await screen.findByRole("region", { name: "WH-1000XM5" });
    expect(
      within(card).queryByRole("radiogroup", { name: t("ui.headset_mic") }),
    ).toBeNull();
  });

  it("names each device by the side apps record from, without the 16-channel cable", async () => {
    start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    await waitFor(() =>
      expect([...micSelect(wh).options].map((o) => o.textContent)).toEqual([
        "CABLE Output (VB-Audio Virtual Cable)",
        "Voicemeeter Out B1 (VB-Audio Voicemeeter VAIO)",
      ]),
    );
    expect(micSelect(wh).value).toBe(CABLE);
  });

  it("lists VB-Cable first and the Hi-Fi Cable after, keeping the device chosen", async () => {
    start("#/devices", "voice");
    mock!.hifiCable = true;
    const wh = await deviceEntry(/WH-1000XM5/);
    await waitFor(() =>
      expect([...micSelect(wh).options].map((o) => o.textContent)).toEqual([
        "CABLE Output (VB-Audio Virtual Cable)",
        "Voicemeeter Out B1 (VB-Audio Voicemeeter VAIO)",
        "Hi-Fi Cable Output (VB-Audio Hi-Fi Cable)",
      ]),
    );
    expect(micSelect(wh).value).toBe(CABLE);
  });

  it("says nothing about the cables below the sources (the banner's Learn more does)", async () => {
    start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    // The microphone off: VB-Cable is free to be the source
    fireEvent.click(mode(wh, "off"));
    const source = () => within(wh).getByLabelText(t("capture.mode")) as HTMLSelectElement;
    await within(source()).findByRole("option", { name: "CABLE Input (VB-Audio Virtual Cable)" });
    fireEvent.change(source(), { target: { value: `virtual:${CABLE}` } });
    await waitFor(() => expect(source().value).toBe(`virtual:${CABLE}`));
    expect(within(wh).queryByRole("button", { name: t("hificable.download") })).toBeNull();
    expect(wh.textContent).not.toContain("Hi-Fi Cable is");
  });

  it("says when the Hi-Fi Cable's two sides run at different rates", async () => {
    start("#/devices", "voice");
    mock!.hifiCable = true;
    mock!.hifiCaptureRate = 44100;
    const wh = await deviceEntry(/WH-1000XM5/);
    const hifi = await waitFor(() => {
      const o = [...micSelect(wh).options].find((x) => /Hi-Fi/.test(x.textContent ?? ""));
      expect(o).toBeTruthy();
      return o!;
    });
    fireEvent.change(micSelect(wh), { target: { value: hifi.value } });
    await waitFor(() => expect(wh.textContent).toContain(t("capture.hifi_rates_differ", 48000, 44100)));
  });

  it("adds the 16-channel cable's own name with every device shown, not a second CABLE Output", async () => {
    const { core } = start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    // Developer mode alone keeps the short list (issue #86)
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    await waitFor(() => expect(micSelect(wh).value).toBe(CABLE));
    expect(micSelect(wh).querySelector("optgroup")).toBeNull();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true, all_audio_devices: true },
    });
    await waitFor(() => {
      const labels = [...micSelect(wh).options].map((o) => o.textContent);
      expect(labels).toContain(
        "CABLE Output (VB-Audio Virtual Cable) · CABLE In 16ch",
      );
      expect(
        labels.filter((l) => l === "CABLE Output (VB-Audio Virtual Cable)"),
      ).toHaveLength(1);
    });
    const output = [...micSelect(wh).options].find(
      (o) => o.textContent === "CABLE Output (VB-Audio Virtual Cable)",
    );
    expect(output?.value).toBe(CABLE);
    // Speakers only play the microphone: under Monitoring, not with the
    // devices apps record from (issue #86)
    const speakers = [...micSelect(wh).options].find(
      (o) => o.value === "{0.0.0.00000000}.{speakers}",
    );
    expect(speakers?.closest("optgroup")?.label).toBe(
      t("capture.mic_monitoring"),
    );
    expect(output?.closest("optgroup")?.label).toBe(t("capture.virtual"));
    // Each group once, the 16-channel cable last although Windows lists it
    // first
    const groups = [...micSelect(wh).querySelectorAll("optgroup")].map((g) => [
      g.label,
      [...g.querySelectorAll("option")].map((o) => o.textContent),
    ]);
    expect(groups).toEqual([
      [
        t("capture.virtual"),
        [
          "CABLE Output (VB-Audio Virtual Cable)",
          "Voicemeeter Out B1 (VB-Audio Voicemeeter VAIO)",
          "CABLE Output (VB-Audio Virtual Cable) · CABLE In 16ch",
        ],
      ],
      [
        t("capture.mic_monitoring"),
        [
          expect.stringContaining("Speakers (Realtek(R) Audio)"),
          "M28U (NVIDIA High Definition Audio)",
        ],
      ],
    ]);
  });

  it("saves the mode and the device with the device's profile", async () => {
    const { saves } = start("#/devices", "voice");
    const xb = await deviceEntry(/SRS-XB13/);
    await waitFor(() => expect(mode(xb, "off")).toBeChecked());
    // There while Off too, so that switching moves nothing (issue #86)
    expect(micSelect(xb)).toBeDisabled();
    expect(
      within(xb).getByText(
        new RegExp(t("ui.headset_mic_quality").replace(/[.]/g, "\\.")),
      ),
    ).toBeInTheDocument();
    fireEvent.click(mode(xb, "on"));
    await waitFor(() =>
      expect(saves().at(-1)).toMatchObject({
        device_name: "SRS-XB13",
        headset_mic: "on",
        mic_device_id: "",
      }),
    );
    await within(xb).findByText(t("ui.headset_mic_no_output"));
    await waitFor(() => expect(micSelect(xb)).toBeEnabled());
    const select = micSelect(xb);
    select.selectedIndex = [...select.options].findIndex(
      (o) => o.value === VOICEMEETER,
    );
    fireEvent.change(select);
    await waitFor(() =>
      expect(saves().at(-1)).toMatchObject({
        device_name: "SRS-XB13",
        headset_mic: "on",
        mic_device_id: VOICEMEETER,
        mic_device_name: "Voicemeeter Input (VB-Audio Voicemeeter VAIO)",
      }),
    );
    await waitFor(() =>
      expect(within(xb).queryByText(t("ui.headset_mic_no_output"))).toBeNull(),
    );
  });

  it("keeps the device and the mode for Off (a mute), disabled", async () => {
    const { saves } = start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(mode(wh, "on")).toBeChecked());
    fireEvent.click(mode(wh, "auto"));
    await waitFor(() =>
      expect(saves().at(-1)).toMatchObject({
        device_name: "WH-1000XM5",
        headset_mic: "auto",
        mic_device_id: CABLE,
      }),
    );
    // Off mutes, as the cards' button does: one state (issue #86)
    await waitFor(() => expect(mode(wh, "auto")).toBeChecked());
    await waitFor(() => expect(mode(wh, "off")).toBeEnabled());
    fireEvent.click(mode(wh, "off"));
    await waitFor(() =>
      expect(saves().at(-1)).toMatchObject({
        headset_mic: "auto",
        mic_muted: true,
        mic_device_id: CABLE,
      }),
    );
    await waitFor(() => expect(mode(wh, "off")).toBeChecked());
    await waitFor(() => expect(micSelect(wh)).toBeDisabled());
    expect(micSelect(wh).value).toBe(CABLE);
  });

  it("processes the microphone while it is not Off, saved with the device's profile", async () => {
    const { saves } = start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(mode(wh, "on")).toBeChecked());
    // A line under Input Source, the controls in a dialog
    expect(within(wh).queryByRole("slider", { name: t("ui.mic_gain") })).toBeNull();
    const row = within(wh).getByRole("button", { name: new RegExp(`^${t("ui.mic_processing")}`) });
    expect(row).toHaveTextContent(t("ui.state_off"));
    fireEvent.click(row);
    const dialog = await screen.findByRole("dialog", { name: t("ui.mic_processing") });
    const gain = within(dialog).getByRole("slider", { name: t("ui.mic_gain") }) as HTMLInputElement;
    const gate = within(dialog).getByRole("slider", { name: t("ui.mic_gate") }) as HTMLInputElement;
    // All off by default: the switch, and the sliders greyed out under it
    const toggle = within(dialog).getByRole("switch", { name: t("ui.mic_processing_enable") });
    expect(toggle).not.toBeChecked();
    expect(gain).toBeDisabled();
    fireEvent.click(toggle);
    await waitFor(() => expect(saves().at(-1)?.mic_processing.enabled).toBe(true));
    await waitFor(() => expect(gain).toBeEnabled());
    expect(gain.value).toBe("0");
    expect(gate).toHaveAttribute("aria-valuetext", t("ui.mic_gate_off"));
    const suppression = within(dialog).getByRole("slider", { name: t("ui.mic_noise_suppression") });
    expect(suppression).toHaveAttribute("aria-valuetext", t("ui.mic_suppression_off"));
    // A slider saves when let go
    fireEvent.input(gain, { target: { value: "6" } });
    expect(gain).toHaveAttribute("aria-valuetext", "+6 dB");
    expect(saves().some((p) => p.mic_processing.gain_db === 6)).toBe(false);
    fireEvent.change(gain);
    await waitFor(() =>
      expect(saves().at(-1)).toMatchObject({
        headset_mic: "on",
        mic_device_id: CABLE,
        mic_processing: { enabled: true, gain_db: 6, suppression: 0, gate_db: -80 },
      }),
    );
    await waitFor(() => expect(gain).toBeEnabled());
    // Noise suppression: off at 0, as the gate is at its lowest
    await waitFor(() => expect(suppression).toBeEnabled());
    fireEvent.input(suppression, { target: { value: "70" } });
    fireEvent.change(suppression);
    await waitFor(() => expect(saves().at(-1)?.mic_processing).toEqual({ enabled: true, gain_db: 6, suppression: 70, gate_db: -80 }),
    );
    expect(suppression).toHaveAttribute("aria-valuetext", "70%");
    await waitFor(() => expect(gate).toBeEnabled());
    fireEvent.input(gate, { target: { value: "-50" } });
    fireEvent.change(gate);
    await waitFor(() => expect(saves().at(-1)?.mic_processing.gate_db).toBe(-50));
    await waitFor(() => expect(row).toHaveTextContent(t("ui.state_on")));
    // Switched off: the values stay, greyed out
    await waitFor(() => expect(toggle).toBeEnabled());
    fireEvent.click(toggle);
    await waitFor(() =>
      expect(saves().at(-1)?.mic_processing).toEqual({ enabled: false, gain_db: 6, suppression: 70, gate_db: -50 }),
    );
    await waitFor(() => expect(gain).toBeDisabled());
    expect(gain.value).toBe("6");
    await waitFor(() => expect(row).toHaveTextContent(t("ui.state_off")));
    // Off keeps the line, disabled (the microphone is closed)
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: t("ui.mic_processing") })).toBeNull());
    await waitFor(() => expect(mode(wh, "off")).toBeEnabled());
    fireEvent.click(mode(wh, "off"));
    await waitFor(() => expect(row).toBeDisabled());
  });

  it("makes the microphone's cable the default input while connected, a switch in developer mode", async () => {
    const { core, saves } = start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    const toggle = (await within(wh).findByRole("switch", {
      name: t("capture.mic_default_input"),
    })) as HTMLInputElement;
    expect(toggle.checked).toBe(true);
    fireEvent.click(toggle);
    await waitFor(() =>
      expect(saves().at(-1)).toMatchObject({
        device_name: "WH-1000XM5",
        mic_default_input: false,
        mic_device_id: CABLE,
      }),
    );
    await waitFor(() => expect(toggle.checked).toBe(false));
    // Always on outside developer mode (issue #86)
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: false },
    });
    await waitFor(() =>
      expect(within(wh).queryByRole("switch", { name: t("capture.mic_default_input") })).toBeNull(),
    );
  });

  it("says playback quality may drop, for LE Audio devices too", async () => {
    start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(wh.textContent).toContain(t("ui.headset_mic_quality")));
  });

  it("lists only the cables a microphone can use, without the output source's", async () => {
    start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(micSelect(wh).value).toBe(CABLE));
    const source = within(wh).getByLabelText(t("capture.mode")) as HTMLSelectElement;
    const values = (s: HTMLSelectElement) => [...s.options].map((o) => o.value);
    // The microphone's cable is not offered as the source (issue #86)
    await waitFor(() => expect(values(source)).toContain(`virtual:${VOICEMEETER}`));
    expect(values(source)).not.toContain(`virtual:${CABLE}`);
    expect(values(source)).not.toContain("virtual:{0.0.0.00000000}.{cable16}");
    fireEvent.change(micSelect(wh), { target: { value: VOICEMEETER } });
    await waitFor(() => expect(values(source)).toContain(`virtual:${CABLE}`));
    expect(values(source)).not.toContain(`virtual:${VOICEMEETER}`);
    // The source's cable leaves the microphone's list instead of being disabled
    fireEvent.change(source, { target: { value: `virtual:${CABLE}` } });
    await waitFor(() => expect(values(micSelect(wh))).not.toContain(CABLE));
    expect(values(micSelect(wh))).toEqual([VOICEMEETER]);
    expect([...micSelect(wh).options].some((o) => o.disabled)).toBe(false);
  });

  it("refuses the device the output source plays from", async () => {
    const { core } = start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true, all_audio_devices: true },
    });
    await waitFor(() => expect(micSelect(wh).value).toBe(CABLE));
    const source = within(wh).getByLabelText(
      t("capture.mode"),
    ) as HTMLSelectElement;
    await waitFor(() =>
      expect(
        [...source.options].some((o) => o.value === `virtual:${CABLE}`),
      ).toBe(true),
    );
    fireEvent.change(source, { target: { value: `virtual:${CABLE}` } });
    await waitFor(() =>
      expect(
        [...micSelect(wh).options].find((o) => o.value === CABLE)?.disabled,
      ).toBe(true),
    );
    await within(wh).findByText(t("capture.mic_loop_here"));
    // Why it cannot be chosen, on the entry itself (issue #86)
    const cable = [...micSelect(wh).options].find((o) => o.value === CABLE);
    expect(cable?.textContent).toBe(
      t("capture.mic_used_by_source", "CABLE Output (VB-Audio Virtual Cable)"),
    );
  });

  it("warns when Windows plays into the microphone's device (issue #86)", async () => {
    const { core } = start("#/devices", "voice");
    mock!.defaultOutput = CABLE;
    const wh = await deviceEntry(/WH-1000XM5/);
    const source = within(wh).getByLabelText(t("capture.mode")) as HTMLSelectElement;
    const DISPLAY = "{0.0.0.00000000}.{display}";
    await waitFor(() =>
      expect([...source.options].some((o) => o.value === `output:${DISPLAY}`)).toBe(true),
    );
    // An output device as the source: Windows still plays into the cable
    fireEvent.change(source, { target: { value: `output:${DISPLAY}` } });
    await within(wh).findByText(t("capture.mic_is_default_output"));
    // A virtual source made the default while connected: no warning
    fireEvent.change(source, { target: { value: `virtual:${VOICEMEETER}` } });
    const autoSwitch = (await within(wh).findByRole("switch", { name: t("capture.auto_switch") })) as HTMLInputElement;
    if (!autoSwitch.checked) fireEvent.click(autoSwitch);
    await waitFor(() =>
      expect(within(wh).queryByText(t("capture.mic_is_default_output"))).toBeNull(),
    );
    // Turned off (developer mode): the warning again
    fireEvent.click(autoSwitch);
    await within(wh).findByText(t("capture.mic_is_default_output"));
    // Outside developer mode the switch is always on (issue #86)
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: false },
    });
    await waitFor(() =>
      expect(within(wh).queryByText(t("capture.mic_is_default_output"))).toBeNull(),
    );
    expect(within(wh).queryByRole("switch", { name: t("capture.auto_switch") })).toBeNull();
  });

  it("refuses the other side of the cable the output source plays to", async () => {
    const { core } = start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true, all_audio_devices: true },
    });
    const source = within(wh).getByLabelText(
      t("capture.mode"),
    ) as HTMLSelectElement;
    await waitFor(() =>
      expect(
        [...source.options].some((o) => o.value === `virtual:${CABLE}`),
      ).toBe(true),
    );
    fireEvent.change(source, { target: { value: `virtual:${CABLE}` } });
    // "CABLE In 16ch" records into the same CABLE Output: mixed with what plays
    await waitFor(() =>
      expect(
        [...micSelect(wh).options].find(
          (o) => o.value === "{0.0.0.00000000}.{cable16}",
        )?.disabled,
      ).toBe(true),
    );
    expect(
      [...micSelect(wh).options].find((o) => o.value === VOICEMEETER)?.disabled,
    ).toBe(false);
  });

  it("warns of call quality for classic and LE Audio devices alike", async () => {
    start("#/devices", "voice");
    const wh = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(mode(wh, "on")).toBeChecked());
    expect(
      within(wh).getByText(
        new RegExp(t("ui.headset_mic_quality").replace(/[.]/g, "\\.")),
      ),
    ).toBeInTheDocument();
    // Listed although no stack checked the adapter yet, as on Home
    const le = [
      ...document.querySelectorAll<HTMLElement>("main .device-entry"),
    ].find((e) => /Galaxy Buds2 Pro/.test(e.textContent ?? "") && micModes(e));
    expect(le).toBeDefined();
    expect(
      within(le!).getByText(
        new RegExp(t("ui.headset_mic_quality").replace(/[.]/g, "\\.")),
      ),
    ).toBeInTheDocument();
  });
});

describe("status", () => {
  const homeCard = (name: string) => screen.findByRole("group", { name });

  it("shows the microphone's line in the sidebar and on Home's card, the codec dimmed", async () => {
    const { core } = start("#/home", "voice");
    const wh = await homeCard("WH-1000XM5");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    const pill = () => document.querySelector(".status-pill .mic-badge");
    await waitFor(() => expect(pill()).toHaveClass("open"));
    expect(pill()).toHaveTextContent(`${t("ui.mic_label")}mSBC16 kHz`);
    // A classic device's codec is paused for the call link (issue #86)
    expect(document.querySelector(".status-pill .codec-info")).toHaveClass("codec-paused");
    expect(wh.querySelector(".mic-row .mic-badge.open")).not.toBeNull();
    expect(wh.querySelector(".codec-info")).toHaveClass("codec-paused");
    await waitFor(() =>
      expect(within(wh).getByRole("button", { name: MUTE() })).toHaveAttribute(
        "title",
        `${MUTE()} (mSBC)`,
      ),
    );
    // Disabled: the line stays, dimmed, and the codec plays again
    fireEvent.click(within(wh).getByRole("button", { name: MUTE() }));
    await waitFor(() => expect(wh.querySelector(".mic-badge")).not.toHaveClass("open"));
    expect(wh.querySelector(".mic-badge")).toHaveTextContent(t("ui.headset_mic_off"));
    await waitFor(() => expect(wh.querySelector(".codec-info")).not.toHaveClass("codec-paused"));
  });

  it("disables the microphone from its icon in the sidebar, as on Home's card (issue #86)", async () => {
    const { saves, core } = start("#/settings", "voice");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    const pill = await waitFor(() => {
      const p = document.querySelector<HTMLElement>(".status-pill.st-streaming");
      expect(p).not.toBeNull();
      expect(p!.querySelector(".mic-badge.open")).not.toBeNull();
      return p!;
    });
    const chip = within(pill).getByRole("button", { name: MUTE() });
    fireEvent.click(chip);
    await waitFor(() => expect(pill.querySelector(".mic-badge")).not.toHaveClass("open"));
    expect(pill.querySelector(".mic-badge")).toHaveTextContent(t("ui.headset_mic_off"));
    // The icon does not open Home
    expect(location.hash).toBe("#/settings");
    const muted = await within(pill).findByRole("button", { name: UNMUTE() });
    await waitFor(() => expect(muted).toBeEnabled());
    fireEvent.click(muted);
    await waitFor(() => expect(saves().at(-1)).toMatchObject({ headset_mic: "on", mic_muted: false }));
    await within(pill).findByRole("button", { name: MUTE() });
  });

  it("drops the voice codec when headset mode is turned off while streaming", async () => {
    const { core } = start("#/devices", "voice");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() =>
      expect(core.state.snap?.stream.voice?.codec).toBe("msbc"),
    );
    const wh = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(mode(wh, "on")).toBeChecked());
    fireEvent.click(mode(wh, "off"));
    await waitFor(() =>
      expect(core.state.snap?.stream.voice ?? null).toBeNull(),
    );
    expect(core.state.snap?.stream.state).toBe("streaming");
  });

  it("measures the call link on Diagnostics as it measures A2DP, with the microphone's packets", async () => {
    const { core } = start("#/diagnostics", "voice");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() =>
      expect(core.state.snap?.stream.voice?.codec).toBe("msbc"),
    );
    const row = (label: string) =>
      screen.getByText(label, { selector: "dt" }).closest(".lq-row")!;
    await waitFor(() =>
      expect(row(t("quality.codec"))).toHaveTextContent(
        t("ui.diag.voice_codec_value", "mSBC", 16),
      ),
    );
    // The SCO payloads sent, not 0
    await waitFor(() =>
      expect(row(t("quality.bitrate"))).toHaveTextContent(
        t("quality.bitrate_value", 64),
      ),
    );
    const mic = await screen.findByRole("heading", {
      name: t("ui.diag.mic_title"),
    });
    await waitFor(() =>
      expect(row(t("ui.diag.mic_received"))).not.toHaveTextContent(/^\D*0$/),
    );
    expect(mic.closest("section")).toHaveTextContent(t("ui.diag.mic_damaged"));
  });

  it("keeps LE Audio's codec as it is while the microphone is open", async () => {
    const { core } = start("#/home", "voice");
    await homeCard("Galaxy Buds2 Pro");
    await core.request("stream.start", { profile: "Buds (LE Audio)" });
    const voice = await waitFor(() => {
      const v = document.querySelector(".status-pill .mic-badge.open");
      expect(v).not.toBeNull();
      return v!;
    });
    expect(voice).toHaveTextContent("LC3");
    expect(document.querySelector(".status-pill .codec-info")).not.toHaveClass("codec-paused");
  });
});

describe("headset chip", () => {
  const homeCard = (name: string) => screen.findByRole("group", { name });

  it("is not on Home with headset mode off", async () => {
    start("#/home");
    const wh = await homeCard("WH-1000XM5");
    expect(wh.querySelector(".headset-chip")).toBeNull();
  });

  it("is only on the cards of devices in use with a microphone output", async () => {
    const { core } = start("#/home", "voice");
    const wh = await homeCard("WH-1000XM5");
    // Not while the device is not connected (issue #86)
    expect(wh.querySelector(".headset-chip")).toBeNull();
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await within(wh).findByRole("button", { name: MUTE() });
    const xb = await homeCard("SRS-XB13");
    expect(xb.querySelector(".headset-chip")).toBeNull();
  });

  it("mutes and unmutes on Home, an icon alone, keeping the mode", async () => {
    const { saves, core } = start("#/home", "voice");
    const wh = await homeCard("WH-1000XM5");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    const on = await within(wh).findByRole("button", { name: MUTE() });
    expect(on).toHaveTextContent("");
    expect(on).toHaveClass("on");
    fireEvent.click(on);
    await waitFor(() =>
      expect(saves().at(-1)).toMatchObject({
        device_name: "WH-1000XM5",
        headset_mic: "on",
        mic_muted: true,
      }),
    );
    const muted = await within(wh).findByRole("button", { name: UNMUTE() });
    expect(muted).toHaveClass("muted");
    expect(muted).not.toHaveClass("on");
    // Each click once the last save is done
    await waitFor(() => expect(muted).toBeEnabled());
    fireEvent.click(muted);
    await waitFor(() =>
      expect(saves().at(-1)).toMatchObject({
        headset_mic: "on",
        mic_muted: false,
      }),
    );
    await within(wh).findByRole("button", { name: MUTE() });
  });

  it("shows Auto waiting with no app recording, and is muted by Off", async () => {
    const { core } = start("#/devices", "voice");
    const entry = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(mode(entry, "on")).toBeChecked());
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    fireEvent.click(mode(entry, "auto"));
    await waitFor(() => expect(mode(entry, "auto")).toBeChecked());
    location.hash = "#/home";
    const wh = await homeCard("WH-1000XM5");
    const auto = await within(wh).findByRole("button", { name: MUTE() });
    expect(auto).toHaveClass("waiting");
    location.hash = "#/devices";
    const again = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(mode(again, "auto")).toBeChecked());
    fireEvent.click(mode(again, "off"));
    await waitFor(() => expect(mode(again, "off")).toBeChecked());
    location.hash = "#/home";
    const card = await homeCard("WH-1000XM5");
    const muted = await within(card).findByRole("button", { name: UNMUTE() });
    expect(muted).toHaveClass("muted");
    // The button opens it again in Auto, and Audio Devices follows
    await waitFor(() => expect(muted).toBeEnabled());
    fireEvent.click(muted);
    await within(card).findByRole("button", { name: MUTE() });
    location.hash = "#/devices";
    const back = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(mode(back, "auto")).toBeChecked());
  });

  it("shows why the microphone did not open on the chip, not as the card's status", async () => {
    const { core } = start("#/home", "voice");
    const wh = await homeCard("WH-1000XM5");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() =>
      expect(core.state.snap?.stream.state).toBe("streaming"),
    );
    mock!.noteStream("error.headset_mic_loop");
    await waitFor(() =>
      expect(wh.querySelector(".headset-chip")).toHaveClass("warn"),
    );
    expect(wh.querySelector(".headset-chip")).toHaveAttribute(
      "title",
      t("error.headset_mic_loop"),
    );
    expect(within(wh).getByRole("status")).toHaveTextContent(
      t("status.connected"),
    );
    expect(wh).not.toHaveTextContent(t("error.headset_mic_loop"));
  });

  it("closes the voice link while muted and unmutes from Audio Devices", async () => {
    const { core, saves } = start("#/home", "voice");
    const wh = await homeCard("WH-1000XM5");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() =>
      expect(core.state.snap?.stream.voice?.codec).toBe("msbc"),
    );
    const chip = await within(wh).findByRole("button", { name: MUTE() });
    await waitFor(() => expect(chip).toHaveClass("linked"));
    fireEvent.click(chip);
    await waitFor(() =>
      expect(core.state.snap?.stream.voice ?? null).toBeNull(),
    );
    expect(core.state.snap?.stream.state).toBe("streaming");
    // Audio Devices shows the mute as Off (issue #86); a mode unmutes
    location.hash = "#/devices";
    const entry = await deviceEntry(/WH-1000XM5/);
    await waitFor(() => expect(micModes(entry)).not.toBeNull());
    await waitFor(() => expect(mode(entry, "off")).toBeChecked());
    fireEvent.click(mode(entry, "auto"));
    await waitFor(() =>
      expect(saves().at(-1)).toMatchObject({
        headset_mic: "auto",
        mic_muted: false,
      }),
    );
  });

  it("is in the flyout and saves from there", async () => {
    const { core, saves } = start("", "voice", "flyout");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    const chip = await screen.findByRole("button", { name: MUTE() });
    expect(chip.closest(".mic-row")).not.toBeNull();
    fireEvent.click(chip);
    await waitFor(() =>
      expect(saves().at(-1)).toMatchObject({
        device_name: "WH-1000XM5",
        mic_muted: true,
      }),
    );
    await screen.findByRole("button", { name: UNMUTE() });
  });

  it("is not in the flyout with headset mode off", async () => {
    const { core } = start("", "normal", "flyout");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() => expect(core.state.snap?.stream.state).not.toBe("idle"));
    expect(document.querySelector(".headset-chip")).toBeNull();
  });
});

describe("call codecs on Codecs (issue #86)", () => {
  const section = (name: string) =>
    waitFor(() => {
      const s = [...document.querySelectorAll<HTMLElement>("section")].find(
        (e) => e.querySelector("h2")?.textContent === name,
      );
      expect(s).toBeTruthy();
      return s!;
    });

  it("shows the call codecs instead of the music ones while the call link is open (issue #86)", async () => {
    const { core } = start("#/codecs", "voice");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() => expect(core.state.snap?.stream.voice?.codec).toBe("msbc"));
    const wh = await section("WH-1000XM5");
    await waitFor(() => expect(wh.querySelector('input[value="ldac"]')).toBeNull());
    const radios = () => [...wh.querySelectorAll<HTMLInputElement>("input[type=radio]")];
    // Only the supported ones (the toggle is on), like the A2DP cards
    expect(radios().map((r) => r.value)).toEqual(["msbc", "cvsd"]);
    expect(wh.querySelectorAll(".codec-card .cbar")).toHaveLength(6);
    expect(radios()[0]).toBeChecked();
    // Auto-select off (the mock's default): they can be chosen
    expect(radios().every((r) => !r.disabled)).toBe(true);
    // "..." shows its settings, read only
    fireEvent.click(within(wh).getByRole("button", { name: t("ui.profile.codec_settings", "mSBC") }));
    const dialog = await screen.findByRole("dialog", { name: t("ui.profile.codec_settings", "mSBC") });
    expect(dialog).toHaveTextContent("16000 Hz");
    expect(dialog).toHaveTextContent("64 kbps");
    // As an A2DP codec's settings
    for (const label of ["connection.quality", "connection.sample_rate", "connection.bit_depth", "ui.diag.cd_channel_mode"])
      expect(within(dialog).getByText(t(label))).toBeInTheDocument();
    fireEvent.keyDown(dialog, { key: "Escape" });
    // All three with the toggle off, each saying whether the headphones list it
    fireEvent.click(screen.getByRole("switch", { name: t("ui.profile.codecs_hide") }));
    await waitFor(() => expect(radios().map((r) => r.value)).toEqual(["lc3_swb", "msbc", "cvsd"]));
    const tag = (v: string) => radios().find((r) => r.value === v)!.closest("label")!.querySelector(".codec-card-tags");
    expect(tag("lc3_swb")).toHaveTextContent(t("ui.profile.codec_not_supported"));
    expect(tag("msbc")).toHaveTextContent(t("ui.profile.codec_supported"));
    expect(tag("cvsd")).toHaveTextContent(t("ui.profile.codec_supported"));
  });

  it("is not there for a device not connected", async () => {
    start("#/codecs", "voice");
    const wh = await section("WH-1000XM5");
    expect(wh.querySelector('input[value="msbc"]')).toBeNull();
    expect(wh.querySelector('input[value="ldac"]')).not.toBeNull();
  });

  it("chooses the call codec with Auto-select off, saved apart from the codec", async () => {
    const { core, saves } = start("#/codecs", "voice");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() => expect(core.state.snap?.stream.voice?.codec).toBe("msbc"));
    const wh = await section("WH-1000XM5");
    const cvsd = await waitFor(() => {
      const r = wh.querySelector<HTMLInputElement>('input[value="cvsd"]');
      expect(r).toBeTruthy();
      return r!;
    });
    // The music cards never show while the call link opens again (A2DP
    // plays in between)
    let musicShown = false;
    const watch = new MutationObserver(() => {
      if (wh.querySelector('input[value="ldac"]')) musicShown = true;
    });
    watch.observe(wh, { childList: true, subtree: true });
    fireEvent.click(cvsd);
    await waitFor(() => expect(saves().at(-1)).toMatchObject({ voice_codec: "cvsd", codec: "ldac" }));
    await waitFor(() => expect(core.state.snap?.stream.voice).toBeNull());
    const radio = (v: string) => wh.querySelector<HTMLInputElement>(`input[value="${v}"]`);
    // Meanwhile: the one chosen, waiting
    expect(radio("cvsd")).toBeChecked();
    expect(radio("cvsd")).toBeDisabled();
    // The link takes it
    await waitFor(() => expect(core.state.snap?.stream.voice?.codec).toBe("cvsd"));
    await waitFor(() => expect(radio("cvsd")).toBeChecked());
    await waitFor(() => expect(radio("cvsd")).not.toBeDisabled());
    watch.disconnect();
    expect(musicShown).toBe(false);
  });

  it("shows the link's call codec under Auto-select and offers to turn it off", async () => {
    const { core, saves, calls } = start("#/codecs", "voice");
    await waitFor(() => expect(core.state.snap).toBeTruthy());
    await core.request("settings.set", { settings: { ...core.state.snap!.settings, auto_codec: true } });
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() => expect(core.state.snap?.stream.voice?.codec).toBe("msbc"));
    const wh = await section("WH-1000XM5");
    const radio = (v: string) => wh.querySelector<HTMLInputElement>(`input[value="${v}"]`);
    await waitFor(() => expect(radio("msbc")).toBeChecked());
    expect(radio("cvsd")).toBeDisabled();
    // A click on a card offers to turn Auto-select off, then chooses it
    fireEvent.click(radio("cvsd")!.closest("label")!);
    const dialog = await screen.findByRole("dialog", { name: t("ui.codecs.auto_dialog_title") });
    fireEvent.click(within(dialog).getByRole("button", { name: t("ui.codecs.auto_dialog_disable") }));
    await waitFor(() => expect(saves().at(-1)).toMatchObject({ voice_codec: "cvsd" }));
    expect(calls.filter((c) => c.method === "settings.set").at(-1)?.params).toMatchObject({
      settings: { auto_codec: false },
    });
  });
});

describe("LE Audio microphone on Codecs (issue #86)", () => {
  const micGrid = () =>
    waitFor(() => {
      const le = screen.getByRole("region", { name: "Galaxy Buds2 Pro" });
      const h = within(le).getByRole("heading", { name: t("ui.diag.mic_title") });
      const grid = h.nextElementSibling as HTMLElement;
      expect(grid).toHaveClass("codec-grid");
      return grid;
    });
  const openMic = async () => {
    fireEvent.click(within(await micGrid()).getByRole("button", { name: t("ui.profile.codec_settings", t("codec.lc3")) }));
    return screen.findByRole("dialog", { name: t("ui.profile.codec_settings", t("codec.lc3")) });
  };

  it("shows the microphone's card while it runs, mono, its rate chosen with Auto-select off", async () => {
    const { core, saves } = start("#/codecs", "voice");
    await screen.findByRole("region", { name: "Galaxy Buds2 Pro" });
    expect(screen.queryByRole("heading", { name: t("ui.diag.mic_title") })).toBeNull();
    await core.request("stream.start", { profile: "Buds (LE Audio)" });
    await waitFor(() => expect(core.state.snap?.stream.voice?.rate).toBe(32000));
    const dialog = await openMic();
    expect(dialog).toHaveTextContent(t("ui.diag.cd_mode.mono"));
    // No stereo to choose, no bit rate to show
    expect(within(dialog).queryByText(t("connection.quality"))).toBeNull();
    const select = within(dialog).getByRole("combobox") as HTMLSelectElement;
    expect([...select.options].map((o) => o.textContent)).toEqual([
      t("codec.auto_value", "32000 Hz"),
      "32000 Hz",
      "16000 Hz",
    ]);
    expect(select.value).toBe("0");
    fireEvent.change(select, { target: { value: "16000" } });
    await waitFor(() => expect(saves().at(-1)).toMatchObject({ le_audio: true, mic_rate: 16000 }));
    await waitFor(() => expect(core.state.snap?.stream.voice?.rate).toBe(16000));
    // The dialog stays open with the rate chosen
    expect(dialog).toBeInTheDocument();
    await waitFor(() => expect((within(dialog).getByRole("combobox") as HTMLSelectElement).value).toBe("16000"));
  });

  it("shows the rate in use under Auto-select, nothing to choose", async () => {
    const { core } = start("#/codecs", "voice");
    await waitFor(() => expect(core.state.snap).toBeTruthy());
    await core.request("settings.set", { settings: { ...core.state.snap!.settings, auto_codec: true } });
    await core.request("stream.start", { profile: "Buds (LE Audio)" });
    await waitFor(() => expect(core.state.snap?.stream.voice?.rate).toBe(32000));
    const dialog = await openMic();
    expect(within(dialog).queryByRole("combobox")).toBeNull();
    expect(dialog).toHaveTextContent("32000 Hz");
  });
});
