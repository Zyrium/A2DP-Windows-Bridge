// LE Audio (experimental) in the UI: no switches in Settings, the LE Audio
// entry of a device on the Codecs screen (LC3) and its GMAP low latency, the
// LE Audio entries of the device list, and the ISO counters of the
// diagnostics

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../app/App";
import { setLanguage, t } from "../i18n";
import { MockCore, type MockScenario } from "../ipc/mock";
import { createCore } from "../ipc/store";

let mock: MockCore | null = null;

function start(hash: string, scenario: MockScenario = "normal", speed = 0) {
  location.hash = hash;
  const calls: { method: string; params: unknown }[] = [];
  mock = new MockCore({ scenario, speed, onCall: (method, params) => calls.push({ method, params }) });
  const core = createCore(mock);
  const view = render(() => <App core={core} />);
  return { calls, core, view };
}

const named = (label: string) => (n: string) => n.startsWith(label);

afterEach(async () => {
  cleanup();
  mock?.dispose();
  mock = null;
  await setLanguage("en");
});

describe("settings", () => {
  it("has no LE Audio or GMAP switch: both are the device's (Codecs)", async () => {
    start("#/settings");
    await screen.findByRole("switch", { name: named(t("ui.settings.developer_mode")) });
    expect(screen.queryByText(t("codec.lc3_ll"))).toBeNull();
  });
});

describe("Codecs screen", () => {
  /** The LE Audio entry of the earbuds on the Codecs screen */
  const leSection = () => screen.queryByRole("region", { name: "Galaxy Buds2 Pro" });

  it("lists the LE Audio entry, LC3 and its low latency card, only with the setting on (issue #86)", async () => {
    start("#/codecs");
    await screen.findByRole("region", { name: "WH-1000XM5" });
    expect(leSection()).toBeNull();
    cleanup();
    mock?.dispose();

    start("#/codecs", "le");
    const le = await screen.findByRole("region", { name: "Galaxy Buds2 Pro" });
    // The LE Audio tag alone, not "Audio" beside it
    expect(le).toHaveTextContent(t("device.tag_le_audio"));
    expect(within(le).queryByText(t("device.tag_audio"))).toBeNull();
    const radios = within(le).getAllByRole("radio").filter((r) => !r.closest(".headset-mic"));
    expect(radios.map((r) => (r as HTMLInputElement).value)).toEqual(["lc3", "lc3_ll"]);
    // Cards like the A2DP ones: three bars each
    expect(le.querySelectorAll(".codec-card .cbar")).toHaveLength(6);
    // LC3 is the default, not LC3 LL (issue #86)
    expect(le.querySelector('input[value="lc3"]')).toBeChecked();
    expect(le.querySelector('input[value="lc3_ll"]')).not.toBeChecked();
    expect(t("codec.lc3_ll")).toBe("LC3 LL");
    // "..." shows the settings as an A2DP codec's, read only
    fireEvent.click(within(le).getByRole("button", { name: t("ui.profile.codec_settings", t("codec.lc3")) }));
    const dialog = await screen.findByRole("dialog", { name: t("ui.profile.codec_settings", t("codec.lc3")) });
    expect(within(dialog).queryByRole("combobox")).toBeNull();
    for (const label of ["connection.quality", "connection.sample_rate", "connection.bit_depth"])
      expect(within(dialog).getByText(t(label))).toBeInTheDocument();
    expect(within(dialog).getByText(t("bitdepth.16"))).toBeInTheDocument();
  });

  it("says the adapter cannot stream LE Audio, or is not checked yet (issue #86)", async () => {
    start("#/codecs", "le_unsupported");
    const le = await screen.findByRole("region", { name: "Galaxy Buds2 Pro" });
    expect(within(le).getByText(t("ui.profile.le_audio_unsupported"))).toBeInTheDocument();
    cleanup();
    mock?.dispose();

    start("#/codecs", "le_unchecked");
    const unchecked = await screen.findByRole("region", { name: "Galaxy Buds2 Pro" });
    expect(within(unchecked).getByText(t("ui.profile.le_audio_unchecked"))).toBeInTheDocument();
  });

  it("chooses LC3 LL (GMAP low latency) and back to LC3, saved with the profile (issue #86)", async () => {
    const { calls } = start("#/codecs", "le");
    const le = await screen.findByRole("region", { name: "Galaxy Buds2 Pro" });
    const saves = () =>
      calls.filter((c) => c.method === "profile.save").map((c) => (c.params as { profile: Record<string, unknown> }).profile);
    fireEvent.click(le.querySelector<HTMLInputElement>('input[value="lc3_ll"]')!);
    await waitFor(() => expect(saves()).toHaveLength(1));
    expect(saves()[0]).toMatchObject({ name: "Buds (LE Audio)", le_audio: true, le_low_latency: true });
    await waitFor(() => expect(le.querySelector<HTMLInputElement>('input[value="lc3_ll"]')).toBeChecked());
    await waitFor(() => expect(le.querySelector<HTMLInputElement>('input[value="lc3"]')).not.toBeDisabled());
    fireEvent.click(le.querySelector<HTMLInputElement>('input[value="lc3"]')!);
    await waitFor(() => expect(saves()).toHaveLength(2));
    expect(saves()[1]).toMatchObject({ name: "Buds (LE Audio)", le_audio: true, le_low_latency: false });
  });

  it("makes the LE Audio profile, named after the device, when LC3 is chosen", async () => {
    const { calls } = start("#/codecs", "le");
    await mock!.call("profile.delete", { name: "Buds (LE Audio)" });
    const le = await screen.findByRole("region", { name: "Galaxy Buds2 Pro" });
    const lc3 = within(le).getByRole("radio", { name: named(t("codec.lc3_ll")) });
    await waitFor(() => expect(lc3).not.toBeChecked());
    fireEvent.click(lc3);
    await waitFor(() => expect(calls.some((c) => c.method === "profile.save")).toBe(true));
    const saved = (calls.find((c) => c.method === "profile.save")!.params as { profile: Record<string, unknown> }).profile;
    expect(saved).toMatchObject({
      name: "Galaxy Buds2 Pro LE Audio",
      device_address: "F4:4E:FD:00:11:22",
      le_audio: true,
      le_low_latency: true,
    });
  });

  it("offers no application capture for an LE Audio device (Audio Devices)", async () => {
    start("#/devices", "le");
    const entry = (await screen.findAllByText(/Galaxy Buds2 Pro/))
      .map((e) => e.closest(".device-entry"))
      .find((e) => e) as HTMLElement;
    const select = within(entry).getByLabelText(t("capture.mode"));
    await within(select).findByRole("option", { name: "CABLE Input (VB-Audio Virtual Cable)" });
    expect(within(select).getByRole("option", { name: t("capture.loopback_default", "Speakers") })).not.toBeDisabled();
    expect(within(select).queryByRole("group", { name: t("capture.app") })).toBeNull();
    // Nor an A2DP packet size
    expect(within(entry).queryByLabelText(t("advanced.max_packet"))).toBeNull();
  });
});

describe("device list", () => {
  it("lists a dual-mode device once with LE Audio on, as its LE Audio entry (issue #86)", async () => {
    start("#/devices", "le");
    fireEvent.click(await screen.findByRole("button", { name: t("connection.scan") }));
    await screen.findByText(/LE Audio Speaker/);
    const rows = within(screen.getByRole("main"))
      .getAllByText(/Galaxy Buds2 Pro/)
      .map((r) => r.closest(".device-row") as HTMLElement);
    expect(rows).toHaveLength(1);
    expect(rows[0].dataset.key).toBe("F4:4E:FD:00:11:22/le");
    expect(rows[0]).toHaveTextContent(t("device.tag_le_audio_set"));
    // Both earbuds' addresses
    expect(rows[0]).toHaveTextContent("F4:4E:FD:00:11:22, F4:4E:FD:00:11:23");
    const tags = [...rows[0].querySelectorAll(".tag")].map((x) => x.textContent);
    expect(tags).not.toContain(t("device.tag_audio"));
    const speaker = screen.getByText(/LE Audio Speaker/).closest(".device-row")!;
    expect(speaker).toHaveTextContent(t("device.tag_le_audio"));
    expect(speaker).not.toHaveTextContent(t("device.tag_le_audio_set"));
  });

  it("adds both sides of a dual-mode device with one Add, and connects it over LE Audio (issue #86)", async () => {
    const { calls } = start("#/devices", "le");
    // Neither side saved yet
    await mock!.call("device.forget", { address: "F4:4E:FD:00:11:22", le_audio: true });
    await mock!.call("device.forget", { address: "F4:4E:FD:00:11:22" });
    calls.length = 0;
    fireEvent.click(await screen.findByRole("button", { name: t("connection.scan") }));
    await screen.findByText(/LE Audio Speaker/);
    const row = within(screen.getByRole("main"))
      .getAllByText(/Galaxy Buds2 Pro/)
      .map((r) => r.closest(".device-row") as HTMLElement)[0];
    fireEvent.click(within(row).getByRole("button", { name: t("ui.devices.add") }));
    await waitFor(() => expect(calls.some((c) => c.method === "stream.start")).toBe(true));
    const renames = calls.filter((c) => c.method === "device.rename").map((c) => c.params as { le_audio?: boolean });
    expect(renames.map((r) => r.le_audio === true).sort()).toEqual([false, true]);
    const made = calls
      .filter((c) => c.method === "profile.save")
      .map((c) => (c.params as { profile: { le_audio: boolean; device_address: string } }).profile);
    expect(made.map((p) => p.le_audio).sort()).toEqual([false, true]);
    expect(made.every((p) => p.device_address === "F4:4E:FD:00:11:22")).toBe(true);
    const started = calls.find((c) => c.method === "stream.start")!.params as { profile: string };
    expect(started.profile).toBe("Galaxy Buds2 Pro LE Audio");
  });

  it("hides the LE Audio entries with LE Audio off", async () => {
    start("#/devices");
    fireEvent.click(await screen.findByRole("button", { name: t("connection.scan") }));
    await screen.findByText(/MX Keys/);
    expect(screen.getAllByText(/Galaxy Buds2 Pro/)).toHaveLength(1);
    expect(screen.queryByText(/LE Audio Speaker/)).toBeNull();
    expect(screen.queryByText(t("device.tag_le_audio"))).toBeNull();
  });

  it("says when it scans for LE Audio devices", async () => {
    // Slower than the other tests: the LE part of the scan lasts long enough to see
    start("#/devices", "le", 0.2);
    fireEvent.click(await screen.findByRole("button", { name: t("connection.scan") }));
    expect(await screen.findByText(t("status.le_scanning"), {}, { timeout: 3000 })).toBeInTheDocument();
    await screen.findByText(/LE Audio Speaker/, {}, { timeout: 3000 });
    expect(screen.queryByText(t("status.le_scanning"))).toBeNull();
  });

  it("stops a running scan with what it found (issue #86)", async () => {
    // Slow enough to see the scan running
    const { calls } = start("#/devices", "le", 0.2);
    fireEvent.click(await screen.findByRole("button", { name: t("connection.scan") }));
    fireEvent.click(await screen.findByRole("button", { name: t("connection.scan_stop") }));
    await waitFor(() => expect(calls.some((c) => c.method === "device.scan_stop")).toBe(true));
    await screen.findByRole("button", { name: t("connection.scan") }, { timeout: 3000 });
    expect(screen.getAllByText(/MX Keys/).length).toBeGreaterThan(0);
  });

  it("forgets and renames the LE Audio entry by address and kind", async () => {
    const { calls } = start("#/devices", "le");
    const le = (await screen.findByText(t("device.tag_le_audio_set"))).closest(".device-row") as HTMLElement;
    fireEvent.click(within(le).getByRole("button", { name: `${t("device.edit_name")}: Galaxy Buds2 Pro` }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.input(within(dialog).getByLabelText(t("device.enter_name")), { target: { value: "Buds LE" } });
    fireEvent.click(within(dialog).getByRole("button", { name: t("modal.save") }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.find((c) => c.method === "device.rename")!.params).toEqual({
      address: "F4:4E:FD:00:11:22",
      name: "Buds LE",
      le_audio: true,
    });

    fireEvent.click(await screen.findByRole("button", { name: `${t("ui.device.forget")}: Buds LE` }));
    const confirm = await screen.findByRole("dialog");
    fireEvent.click(within(confirm).getByRole("button", { name: t("ui.device.forget") }));
    await waitFor(() => expect(screen.queryByText(/Buds LE/)).toBeNull());
    expect(calls.find((c) => c.method === "device.forget")!.params).toEqual({ address: "F4:4E:FD:00:11:22", le_audio: true });
  });
});

describe("diagnostics", () => {
  it("shows an LE Audio stream's ISO counters instead of the BR/EDR band", async () => {
    const { core } = start("#/diagnostics", "le");
    await screen.findByRole("heading", { level: 1, name: "Diagnostics" });
    await waitFor(() => expect(core.state.snap).not.toBeNull());
    expect(screen.queryByRole("heading", { name: t("ui.diag.iso_title") })).toBeNull();
    // The BR/EDR band is on the radio tab, the ISO counters on the stream tab
    fireEvent.click(screen.getByRole("tab", { name: t("ui.diag.tab_radio") }));
    expect(await screen.findByRole("heading", { name: t("quality.section_band") })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: t("quality.section_stream") }));

    await core.request("stream.start", { profile: "Buds (LE Audio)" });
    await waitFor(() => expect(core.state.snap!.stream.state).toBe("streaming"));
    const group = (await screen.findByRole("heading", { name: t("ui.diag.iso_title") })).closest("section")!;
    expect(group).toHaveTextContent("LC3 48 kHz 10 ms 120 octets (96 kbps/ch), 2 ch over 2 CIS, 48_4_2");
    await waitFor(() => expect(within(group).getByText(t("ui.diag.iso_cis")).closest(".lq-row")).toHaveTextContent("2"));
    for (const key of ["ui.diag.iso_sdu_sets", "ui.diag.iso_silence", "ui.diag.iso_underruns", "ui.diag.iso_dropped"]) {
      expect(within(group).getByText(t(key)).closest(".lq-row")).toHaveTextContent(/\d/);
    }
    expect(within(group).getByText(t("ui.diag.iso_prefill")).closest(".lq-row")).toHaveTextContent(/\d+ frames/);
    // The codec row names LC3
    expect(screen.getByText(t("quality.codec")).closest(".lq-row")).toHaveTextContent(t("ui.codec.lc3"));
    // No BR/EDR figures for LE Audio: neither the band nor the radio charts
    fireEvent.click(screen.getByRole("tab", { name: t("ui.diag.tab_radio") }));
    expect(await screen.findByRole("heading", { name: t("quality.title") })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: t("quality.section_band") })).toBeNull();
    expect(screen.queryByRole("img", { name: new RegExp(`^${t("ui.diag.rssi")}`) })).toBeNull();
  });

  it("reports the setting being off when an LE Audio profile starts", async () => {
    const { core } = start("#/", "le");
    await waitFor(() => expect(core.state.snap).not.toBeNull());
    await core.request("settings.set", { settings: { ...core.state.snap!.settings, le_audio_experimental: false } });
    await core.request("stream.start", { profile: "Buds (LE Audio)" });
    await waitFor(() => expect(core.state.snap!.stream.state).toBe("error"));
    expect(core.state.snap!.stream.message.key).toBe("error.le_audio_disabled");
  });

  it("keeps LE Audio to developer mode: no entries, and its profile says so (issue #86)", async () => {
    const { core } = start("#/codecs", "le");
    await waitFor(() => expect(core.state.snap).not.toBeNull());
    await screen.findByRole("region", { name: "Galaxy Buds2 Pro" });
    await core.request("settings.set", { settings: { ...core.state.snap!.settings, developer_mode: false } });
    await waitFor(() => expect(screen.queryByRole("region", { name: "Galaxy Buds2 Pro" })).toBeNull());
    await core.request("stream.start", { profile: "Buds (LE Audio)" });
    await waitFor(() => expect(core.state.snap!.stream.state).toBe("error"));
    expect(core.state.snap!.stream.message.key).toBe("error.le_audio_developer");
  });

  it("shows an LE Audio device's codec as a value, not a disabled list (issue #86)", async () => {
    const { core } = start("#/", "le");
    await waitFor(() => expect(core.state.snap).not.toBeNull());
    await core.request("stream.start", { profile: "Buds (LE Audio)" });
    await waitFor(() => expect(core.state.snap!.stream.state).toBe("streaming"));
    const codec = await waitFor(() => {
      const o = document.querySelector<HTMLOutputElement>("output.codec-picker");
      expect(o).not.toBeNull();
      return o!;
    });
    expect(codec).toHaveTextContent(t("ui.codec.lc3"));
    expect(codec).toHaveAccessibleName(t("quality.codec"));
    expect(document.querySelector(".codec-picker select:disabled")).toBeNull();
  });
});
