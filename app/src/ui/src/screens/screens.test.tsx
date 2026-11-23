import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../app/App";
import { ICONS } from "../components/icons";
import { FALLBACK_NOTE_MS } from "../components/stream";
import { setLanguage, t } from "../i18n";
import { MockCore, type MockOptions } from "../ipc/mock";
import type { Snapshot } from "../ipc/types";
import { createCore } from "../ipc/store";
import { endSettle } from "./Codecs";

let mock: MockCore | null = null;

function start(hash: string, opts: MockOptions = {}) {
  location.hash = hash;
  const calls: { method: string; params: unknown }[] = [];
  mock = new MockCore({ speed: 0, ...opts, onCall: (method, params) => calls.push({ method, params }) });
  const core = createCore(mock);
  const view = render(() => <App core={core} />);
  return { core, calls, view };
}

afterEach(async () => {
  cleanup();
  endSettle();
  mock?.dispose();
  mock = null;
  await setLanguage("en");
});

/** A button with the administrator shield (it starts a UAC prompt) */
const shielded = (b: HTMLElement) => b.querySelector("svg.icon path")?.getAttribute("d") === ICONS.shield;

/** A device's card on Home (its button is "Connect" on every card) */
const homeCard = (name: string) => screen.findByRole("group", { name });

describe("Home", () => {
  it("says Hi-Res was turned on in the earbuds until dismissed (issue #237)", async () => {
    start("#/home");
    const wh = await homeCard("WH-1000XM5");
    fireEvent.click(within(wh).getByRole("button", { name: "Connect" }));
    await within(wh).findByRole("button", { name: "Disconnect" });
    expect(screen.queryByText(t("ui.home.hi_res_set"))).toBeNull();
    // The stream the reconnect for it brought
    const m = mock as unknown as { snap: Snapshot; emit: (e: unknown) => void };
    await waitFor(() => expect(m.snap.stream.info).toBeTruthy());
    m.snap.stream = { ...m.snap.stream, info: { ...m.snap.stream.info!, hi_res_set: true } };
    m.emit({ kind: "stream", value: m.snap.stream });
    const text = await screen.findByText(t("ui.home.hi_res_set"));
    fireEvent.click(within(text.closest(".banner") as HTMLElement).getByRole("button", { name: t("modal.close") }));
    await waitFor(() => expect(screen.queryByText(t("ui.home.hi_res_set"))).toBeNull());
  });

  it("warns on the card and in the sidebar while the bit rate is lowered for a slow link (issue #86)", async () => {
    start("#/home");
    const wh = await homeCard("WH-1000XM5");
    fireEvent.click(within(wh).getByRole("button", { name: "Connect" }));
    await within(wh).findByRole("button", { name: "Disconnect" });
    const m = mock as unknown as { snap: Snapshot; emit: (e: unknown) => void };
    await waitFor(() => expect(m.snap.stream.info).toBeTruthy());
    expect(screen.queryByText(t("ui.codec.slow_link"))).toBeNull();
    m.snap.stream = { ...m.snap.stream, slow_link: 330 };
    m.emit({ kind: "stream", value: m.snap.stream });
    await waitFor(() => expect(wh.querySelector(".codec-slow")).toHaveTextContent(t("ui.codec.slow_link")));
    // The bit rate in force, not the one asked for
    expect(wh.querySelector(".codec-badge")).toHaveTextContent("330 kbps");
    expect(document.querySelector(".status-pill .codec-slow")).not.toBeNull();
    // As before again
    m.snap.stream = { ...m.snap.stream, slow_link: null };
    m.emit({ kind: "stream", value: m.snap.stream });
    await waitFor(() => expect(screen.queryByText(t("ui.codec.slow_link"))).toBeNull());
    expect(wh.querySelector(".codec-badge")).toHaveTextContent("990 kbps");
  });

  it("drops the driver error once the error's own button put the adapter on WinUSB", async () => {
    start("#/home");
    const wh = await homeCard("WH-1000XM5");
    const m = mock as unknown as { snap: Snapshot; emit: (e: unknown) => void };
    m.snap.adapter = { ...m.snap.adapter, adapters: m.snap.adapter.adapters.map((a) => ({ ...a, driver: "bluetooth" as const })) };
    m.emit({ kind: "adapter", value: m.snap.adapter });
    fireEvent.click(within(wh).getByRole("button", { name: "Connect" }));
    const replace = await screen.findByRole("button", { name: "Replace with WinUSB driver" });
    expect(shielded(replace)).toBe(true);
    fireEvent.click(replace);
    const dialog = await screen.findByRole("dialog");
    const confirm = within(dialog).getByRole("button", { name: "Replace with WinUSB driver" });
    expect(shielded(confirm)).toBe(true);
    fireEvent.click(confirm);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Replace with WinUSB driver" })).toBeNull());
    expect(document.querySelector(".banner.error, .banner-error")).toBeNull();
  });

  it("connects and disconnects with the device's own button", async () => {
    const { calls } = start("#/home");
    const wh = await homeCard("WH-1000XM5");
    const xb = await homeCard("SRS-XB13");
    fireEvent.click(within(wh).getByRole("button", { name: "Connect" }));
    // The mock goes connecting -> streaming
    const disconnect = await within(wh).findByRole("button", { name: "Disconnect" });
    expect(calls.find((c) => c.method === "stream.start")?.params).toEqual({ profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() => expect(wh).toHaveTextContent("990 kbps"));
    // The badge on Home has the bit depth too, on the line below the codec (issue #86)
    expect(wh.querySelector(".codec-badge")).toHaveTextContent("24-bit");
    // The codec is chosen right on the card, by name only (issue #86)
    expect(within(wh).getByRole("combobox", { name: "Codec" })).toHaveDisplayValue("LDAC");
    expect(within(wh).getByRole("status")).toHaveTextContent(/^Connected$/);
    expect(within(wh).getByRole("status").querySelector(".dot")).not.toBeNull();
    // The other device: ready, not connected, its own Connect
    expect(xb).toHaveTextContent("Disconnected");
    expect(xb).not.toHaveTextContent("—");
    expect(within(xb).getByRole("button", { name: "Connect" })).toBeInTheDocument();
    // The volume is the device's own; the other card keeps its slider,
    // disabled, so the cards stay alike (issue #86)
    expect(within(wh).getByRole("slider", { name: "Volume" })).not.toBeDisabled();
    expect(within(xb).getByRole("slider", { name: "Volume" })).toBeDisabled();
    expect(within(xb).queryByRole("combobox", { name: "Codec" })).toBeNull();
    fireEvent.click(disconnect);
    await within(wh).findByRole("button", { name: "Connect" });
    expect(calls.some((c) => c.method === "stream.stop")).toBe(true);
  });

  it("mutes and unmutes with the speaker, as Windows does (issue #86)", async () => {
    const { calls } = start("#/home");
    const wh = await homeCard("WH-1000XM5");
    fireEvent.click(within(wh).getByRole("button", { name: "Connect" }));
    await within(wh).findByRole("button", { name: "Disconnect" });
    const slider = within(wh).getByRole("slider", { name: "Volume" }) as HTMLInputElement;
    await waitFor(() => expect(Number(slider.value)).toBeGreaterThan(0));
    const before = Number(slider.value);
    const volumes = () => calls.filter((c) => c.method === "stream.set_volume").map((c) => (c.params as { volume: number }).volume);
    fireEvent.click(within(wh).getByRole("button", { name: "Mute" }));
    await waitFor(() => expect(volumes().at(-1)).toBe(0));
    const unmute = await within(wh).findByRole("button", { name: "Unmute" });
    expect(unmute).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(unmute);
    await waitFor(() => expect(volumes().at(-1)).toBeGreaterThan(0));
    await waitFor(() => expect(Number(slider.value)).toBe(before));
    // Not on the card of a device not connected
    const xb = await homeCard("SRS-XB13");
    expect(within(xb).getByRole("button", { name: "Mute" })).toBeDisabled();
  });

  it("shows the battery as unknown, with the reason on hover, when the headphones refuse to report it", async () => {
    start("#/home", { scenario: "battery_refused" });
    const wh = await homeCard("WH-1000XM5");
    fireEvent.click(within(wh).getByRole("button", { name: "Connect" }));
    await within(wh).findByRole("button", { name: "Disconnect" });
    const battery = await waitFor(() => {
      const b = wh.querySelector(".battery");
      expect(b).not.toBeNull();
      return b!;
    });
    expect(battery.getAttribute("aria-label")).toMatch(/^Battery unknown/);
    expect(battery.getAttribute("title")).toMatch(/Disconnect them from other devices/);
    expect(battery).not.toHaveTextContent("%");
  });

  it("shows the link quality, not a codec card, the link latency nor the capture source (issue #86)", async () => {
    const { core } = start("#/home");
    const wh = await homeCard("WH-1000XM5");
    const main = within(screen.getByRole("main"));
    // Only while it streams: after that there is no link to rate
    expect(main.queryByRole("region", { name: t("quality.title") })).toBeNull();
    await core.request("stream.start", { profile: core.state.snap!.active_profile! });
    const quality = await within(wh).findByRole("region", { name: t("quality.title") });
    expect(quality).toBeInTheDocument();
    // The codec is on the device's card
    expect(main.queryByRole("region", { name: t("quality.codec") })).toBeNull();
    expect(main.queryByText("Link latency")).toBeNull();
    expect(main.queryByLabelText(t("capture.mode"))).toBeNull();
    await core.request("stream.stop");
    await waitFor(() => expect(main.queryByRole("region", { name: t("quality.title") })).toBeNull());
  });

  it("offers to add a device beside a lone card, not beside two (issue #86)", async () => {
    const { core } = start("#/home");
    await homeCard("WH-1000XM5");
    const add = () => screen.queryByRole("button", { name: t("ui.home.add_device") });
    expect(document.querySelectorAll(".home-device").length).toBeGreaterThan(1);
    expect(add()).toBeNull();
    const devices = core.state.snap!.devices.filter((d) => d.saved && !d.name.startsWith("WH-1000XM5"));
    for (const d of devices) await core.request("device.forget", { address: d.address, le_audio: d.le_audio });
    await waitFor(() => expect(document.querySelectorAll(".home-device")).toHaveLength(1));
    fireEvent.click(add()!);
    await waitFor(() => expect(location.hash).toBe("#/devices"));
  });

  it("switches to another device with its Connect (one device streams at a time)", async () => {
    const { calls } = start("#/home");
    const wh = await homeCard("WH-1000XM5");
    const xb = await homeCard("SRS-XB13");
    fireEvent.click(within(wh).getByRole("button", { name: "Connect" }));
    await within(wh).findByRole("button", { name: "Disconnect" });
    fireEvent.click(within(xb).getByRole("button", { name: "Connect" }));
    await within(xb).findByRole("button", { name: "Disconnect" });
    expect(calls.filter((c) => c.method === "stream.start").map((c) => c.params)).toEqual([
      { profile: "WH-1000XM5 (LDAC)" },
      { profile: "Desk speaker (AAC)" },
    ]);
    // stream.start stops the running stream itself
    expect(calls.some((c) => c.method === "stream.stop")).toBe(false);
    expect(within(wh).getByRole("button", { name: "Connect" })).toBeInTheDocument();
  });

  it("gives a device without a profile one (Auto) when connected", async () => {
    const { calls } = start("#/home");
    await homeCard("WH-1000XM5");
    await mock!.call("device.rename", { address: "54:2F:04:4B:29:DC", name: "moto buds+" });
    const moto = await homeCard("moto buds+");
    fireEvent.click(within(moto).getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(calls.some((c) => c.method === "stream.start")).toBe(true));
    expect(calls.find((c) => c.method === "profile.save")?.params).toMatchObject({
      profile: { name: "moto buds+ | Auto", codec: "auto", device_address: "54:2F:04:4B:29:DC" },
      original_name: null,
    });
    expect(calls.find((c) => c.method === "stream.start")?.params).toEqual({ profile: "moto buds+ | Auto" });
  });

  it("shows each device, and the codec and its details of the one streaming, in the sidebar on every screen (issue #86)", async () => {
    const { core } = start("#/settings");
    const pills = () => [...document.querySelectorAll(".status-pill")] as HTMLElement[];
    await waitFor(() => expect(pills()).toHaveLength(2));
    const [wh, xb] = pills();
    expect(wh).toHaveTextContent("WH-1000XM5");
    expect(wh).not.toHaveTextContent("AC:80:0A:12:34:56");
    expect(wh).toHaveTextContent("Disconnected");
    expect(xb).toHaveTextContent("SRS-XB13");
    await core.request("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() => expect(wh.querySelector(".codec-name")).toHaveTextContent(/^LDAC$/));
    // Streaming reads "Connected", as on Home and in the tray (issue #86)
    expect(wh.querySelector(".status-pill-state")).toHaveTextContent(/^Connected$/);
    expect(wh).not.toHaveTextContent("Streaming");
    // Two lines: "LDAC", then "990 kbps 96 kHz 24-bit" in the badge below it
    const badge = wh.querySelector(".codec-line .codec-badge")!;
    expect(badge).not.toHaveTextContent("LDAC");
    expect([...badge.children].map((c) => c.textContent)).toEqual(["990 kbps", expect.stringMatching(/^[\d.]+ kHz$/), "24-bit"]);
    expect(wh.querySelector(".status-pill-open")).toHaveAccessibleName(/^WH-1000XM5, Connected, LDAC 990 kbps/);
    // The other one: idle, no codec
    expect(xb).toHaveTextContent("Disconnected");
    expect(xb.querySelector(".codec-info")).toBeNull();
  });

  it("says why another codec is used on the codec's line, not the state line (issue #86)", async () => {
    // "long": LDAC chosen, in use elsewhere, aptX HD used
    const { core } = start("#/home", { scenario: "long" });
    await waitFor(() => expect(core.state.snap).toBeTruthy());
    await core.request("stream.start", { profile: core.state.snap!.active_profile! });
    const pill = await waitFor(() => {
      const p = document.querySelector(".status-pill.st-streaming");
      expect(p).not.toBeNull();
      return p as HTMLElement;
    });
    const card = document.querySelector(".home-device.live") as HTMLElement;
    const note = t("ui.codec.fallback_in_use", "LDAC");
    await waitFor(() => expect(pill.querySelector(".codec-name")).toHaveTextContent(`aptX HD${note}`));
    // On Home the codec is chosen on the card: the note beside the choice
    expect(await within(card).findByRole("combobox")).toHaveDisplayValue("aptX HD");
    for (const where of [card, pill]) {
      const name = where.querySelector(".codec-name")!;
      expect(name.querySelector(".icon")).not.toBeNull();
      expect(name.querySelector(".codec-note")).toHaveTextContent(note);
    }
    expect(within(card).getByRole("status")).toHaveTextContent(new RegExp(`^${t("status.connected")}$`));
    expect(pill.querySelector(".status-pill-state")).toHaveTextContent(new RegExp(`^${t("status.connected")}$`));
    // For a few seconds only, then the icon alone, its tooltip saying why
    await waitFor(() => expect(card.querySelector(".codec-note")).toBeNull(), { timeout: FALLBACK_NOTE_MS + 2000 });
    for (const where of [card, pill]) {
      expect(where.querySelector(".codec-note")).toBeNull();
      expect(where.querySelector(".codec-name .icon")).not.toBeNull();
    }
  }, 15000);

  it("chooses the codec on the card of the device in use, by name only (issue #86)", async () => {
    const { calls, core } = start("#/home");
    const wh = await homeCard("WH-1000XM5");
    expect(screen.queryByRole("heading", { name: "Output Source" })).toBeNull();
    // Not on a device that is not connected
    expect(screen.queryByRole("combobox", { name: "Codec" })).toBeNull();
    fireEvent.click(within(wh).getByRole("button", { name: "Connect" }));
    const select = (await within(wh).findByRole("combobox", { name: "Codec" })) as HTMLSelectElement;
    // The codecs the device supports in its order, no Auto-select entry
    expect([...select.options].map((o) => o.value)).toEqual(["ldac", "aac", "sbc"]);
    expect(select.value).toBe("ldac");
    // Saved at once, as a card chosen on Codecs
    fireEvent.change(select, { target: { value: "aac" } });
    await waitFor(() =>
      expect(calls.find((c) => c.method === "profile.save")?.params).toMatchObject({
        profile: { codec: "aac" },
        original_name: "WH-1000XM5 (LDAC)",
      }),
    );
    await waitFor(() => expect(select.value).toBe("aac"));
    // Under Auto-select (turned on on Codecs) it shows the codec streaming,
    // by name, no "Auto-select" (the mock keeps streaming AAC)
    await core.request("settings.set", { settings: { ...core.state.snap!.settings, auto_codec: true } });
    await waitFor(() => expect(core.state.snap!.settings.auto_codec).toBe(true));
    expect(select.selectedOptions[0]).toHaveTextContent(/^AAC$/);
    // A codec chosen under Auto-select turns it off first
    const before = calls.length;
    fireEvent.change(select, { target: { value: "sbc" } });
    const after = () => calls.slice(before).filter((c) => c.method === "settings.set" || c.method === "profile.save");
    await waitFor(() => expect(after().map((c) => c.method)).toEqual(["settings.set", "profile.save"]));
    await waitFor(() => expect(select.value).toBe("sbc"));
    expect(select.selectedOptions[0]).toHaveTextContent(/^SBC$/);
    expect(after()[0].params).toMatchObject({ settings: { auto_codec: false } });
    expect(after()[1].params).toMatchObject({ profile: { codec: "sbc" } });
  });

  it("asks to exit the other BlueCaster (v1, or another v2) while it runs", async () => {
    start("#/home");
    await homeCard("WH-1000XM5");
    expect(screen.queryByText("Another BlueCaster is running")).toBeNull();
    mock!.setV1Running(true);
    expect(await screen.findByText("Another BlueCaster is running")).toBeInTheDocument();
    mock!.setV1Running(false);
    await waitFor(() => expect(screen.queryByText("Another BlueCaster is running")).toBeNull());
  });

  it("asks for a power cycle when the adapter hung (issue #86)", async () => {
    start("#/home");
    await homeCard("WH-1000XM5");
    expect(screen.queryByText("Windows cannot read the Bluetooth adapter")).toBeNull();
    mock!.setHung();
    expect(await screen.findByText("Windows cannot read the Bluetooth adapter")).toBeInTheDocument();
    expect(screen.getByText(/hold Shift while choosing Shut down/)).toBeInTheDocument();
    // Not "check the WinUSB driver" beside it
    expect(screen.queryByText(/Check that it uses the WinUSB driver/)).toBeNull();
  });

  it("shows the cause of an error and a fix", async () => {
    start("#/home", { scenario: "error" });
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("The device did not respond");
    expect(within(alert).getByRole("button", { name: "Try again" })).toBeInTheDocument();
  });

  it("connects a device again after an error instead of offering Disconnect", async () => {
    const { calls } = start("#/home", { scenario: "error" });
    const wh = await homeCard("WH-1000XM5");
    expect(wh).toHaveTextContent("Error");
    // The red word alone: a red dot read as recording (issue #86)
    expect(within(wh).getByRole("status").querySelector(".dot")).toBeNull();
    expect(within(wh).queryByRole("button", { name: "Disconnect" })).toBeNull();
    fireEvent.click(within(wh).getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(calls.some((c) => c.method === "stream.start")).toBe(true));
    expect(calls.some((c) => c.method === "stream.stop")).toBe(false);
  });
});

describe("every screen renders", () => {
  const routes: [string, string][] = [
    ["#/home", "Home"],
    ["#/codecs", "Codecs"],
    // The old Profiles links lead to Codecs
    ["#/profiles", "Codecs"],
    ["#/profiles/new", "Codecs"],
    ["#/devices", "Audio Devices"],
    ["#/diagnostics", "Diagnostics"],
    ["#/adapter", "Adapters"],
    ["#/about", "About"],
    ["#/settings", "Settings"],
  ];
  for (const [hash, heading] of routes) {
    it(hash, async () => {
      start(hash);
      expect(await screen.findByRole("heading", { level: 1, name: heading })).toBeInTheDocument();
      expect(screen.getByRole("navigation")).toBeInTheDocument();
    });
  }

  it("the adapter's driver buttons carry the administrator shield, others not", async () => {
    start("#/adapter");
    const restore = await screen.findByRole("button", { name: t("ui.adapter.restore_driver") });
    expect(shielded(restore)).toBe(true);
    expect(shielded(screen.getByRole("button", { name: t("ui.adapter.restart") }))).toBe(false);
    fireEvent.click(restore);
    const dialog = await screen.findByRole("dialog", { name: t("ui.adapter.restore_title") });
    expect(shielded(within(dialog).getByRole("button", { name: t("ui.adapter.restore_driver") }))).toBe(true);
    expect(shielded(within(dialog).getByRole("button", { name: t("modal.cancel") }))).toBe(false);
  });

  it("diagnostics tabs", async () => {
    start("#/diagnostics");
    // The stream tab first
    expect(await screen.findByRole("tab", { name: "Stream" })).toHaveAttribute("aria-selected", "true");
    for (const name of ["Radio", "Audio capture", "Connection Flow", "Log", "Stream"]) {
      fireEvent.click(await screen.findByRole("tab", { name }));
      expect(screen.getByRole("tab", { name })).toHaveAttribute("aria-selected", "true");
    }
    expect(screen.queryByRole("tab", { name: "Peer Receiver Test" })).toBeNull();
  });

  it("the first-run setup when no adapter is on WinUSB", async () => {
    const { calls } = start("#/home", { scenario: "fresh" });
    expect(await screen.findByRole("heading", { level: 1, name: "Setup Wizard" })).toBeInTheDocument();
    mock!.setV1Running(true);
    expect(await screen.findByText("Another BlueCaster is running")).toBeInTheDocument();
    // The setup waits while the other one holds the adapter
    expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
    mock!.setV1Running(false);
    await waitFor(() => expect(screen.getByRole("button", { name: "Next" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    // Step 2: one explanation and one button, which switches the driver and
    // downloads the firmware (issue #86)
    const setUp = await screen.findByRole("button", { name: "Set up this adapter" });
    // It changes the driver: an administrator prompt
    expect(shielded(setUp)).toBe(true);
    expect(screen.getByText(/removes the adapter from Windows/)).toBeInTheDocument();
    expect(screen.getByText("rtl8761bu_fw.bin, rtl8761bu_config.bin")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Replace with WinUSB driver" })).toBeNull();
    // Another BlueCaster started meanwhile: no driver change until it exits
    mock!.setV1Running(true);
    await waitFor(() => expect(setUp).toBeDisabled());
    mock!.setV1Running(false);
    await waitFor(() => expect(setUp).toBeEnabled());
    fireEvent.click(setUp);
    await screen.findByText("The adapter is ready for BlueCaster", {}, { timeout: 5000 });
    const order = calls.map((c) => c.method).filter((m) => m === "adapter.assign_winusb" || m === "adapter.download_firmware");
    expect(order).toEqual(["adapter.assign_winusb", "adapter.download_firmware"]);
    expect(screen.getByRole("button", { name: "Next" })).not.toBeDisabled();
    // Step 3: no connection while another BlueCaster holds the adapter
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    const connect = (await screen.findAllByRole("button", { name: "Connect" }, { timeout: 5000 }))[0];
    mock!.setV1Running(true);
    await waitFor(() => expect(connect).toBeDisabled());
    mock!.setV1Running(false);
    await waitFor(() => expect(connect).toBeEnabled());
  });

  it("the setup keeps a failed connection's error and starts no scan by itself", async () => {
    const { calls, core } = start("#/home", { scenario: "fresh" });
    fireEvent.click(await screen.findByRole("button", { name: "Next" }));
    fireEvent.click(await screen.findByRole("button", { name: "Set up this adapter" }));
    await screen.findByText("The adapter is ready for BlueCaster", {}, { timeout: 5000 });
    await waitFor(() => expect(screen.getByRole("button", { name: "Next" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    const connect = (await screen.findAllByRole("button", { name: "Connect" }, { timeout: 5000 }))[0];
    mock!.failNextConnect("error.virtual_capture_init");
    fireEvent.click(connect);
    await waitFor(() => expect(core.state.snap!.stream.state).toBe("error"));
    const scans = calls.filter((c) => c.method === "device.scan").length;
    await new Promise((r) => setTimeout(r, 300));
    expect(calls.filter((c) => c.method === "device.scan")).toHaveLength(scans);
    expect(core.state.snap!.stream.state).toBe("error");
    // The Scan button still finds devices
    fireEvent.click(await screen.findByRole("button", { name: "Scan Devices" }));
    await waitFor(() => expect(calls.filter((c) => c.method === "device.scan")).toHaveLength(scans + 1));
  });

  it("opens the setup by itself only until it was finished or skipped once (issue #86)", async () => {
    start("#/home", { scenario: "fresh" });
    // Set up before, every device removed since: Home adds one
    (mock as unknown as { snap: Snapshot }).snap.settings.setup_done = true;
    expect(await screen.findByRole("heading", { level: 1, name: "Home" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { level: 1, name: "Setup Wizard" })).toBeNull();
  });

  it("skipping the setup keeps it from opening by itself again (issue #86)", async () => {
    const { core } = start("#/home", { scenario: "fresh" });
    fireEvent.click(await screen.findByRole("button", { name: t("ui.setup.skip") }));
    expect(await screen.findByRole("heading", { level: 1, name: "Home" })).toBeInTheDocument();
    await waitFor(() => expect(core.state.snap!.settings.setup_done).toBe(true));
  });

  it("a folder set up without the setup does not get it (issue #86)", async () => {
    const { calls, core } = start("#/home");
    (mock as unknown as { snap: Snapshot }).snap.settings.setup_done = false;
    expect(await screen.findByRole("heading", { level: 1, name: "Home" })).toBeInTheDocument();
    await waitFor(() => expect(core.state.snap!.settings.setup_done).toBe(true));
    expect(calls.filter((c) => c.method === "settings.set")).toHaveLength(1);
  });

  it("the setup names the devices Windows loses with the adapter, and asks first (issue #86)", async () => {
    const { calls } = start("#/home", { scenario: "fresh" });
    mock!.adapterDevices = [
      { address: "00:11:22:33:44:01", name: "Logitech K380", audio: false },
      { address: "00:11:22:33:44:02", name: "MX Master 3", audio: false },
      { address: "00:11:22:33:44:03", name: "", audio: true },
      { address: "00:11:22:33:44:04", name: "WH-1000XM4", audio: true },
      { address: "00:11:22:33:44:05", name: "Pixel 8", audio: false },
    ];
    fireEvent.click(await screen.findByRole("button", { name: "Next" }));
    fireEvent.click(await screen.findByRole("button", { name: t("ui.setup.auto_button") }));
    let dialog = await screen.findByRole("dialog", { name: t("ui.adapter.paired_title") });
    // A warning: every device but the audio ones (the setup's import and
    // pairing take those), one per line; the confirm button in the danger style
    expect(dialog.querySelector(".dialog-warn")).not.toBeNull();
    const rows = [...dialog.querySelectorAll(".paired-list li")].map((li) => li.textContent);
    expect(rows).toEqual(["Logitech K380", "MX Master 3", "Pixel 8"]);
    expect(dialog.querySelectorAll(".paired-list li .icon")).toHaveLength(3);
    expect(dialog).toHaveTextContent(t("ui.adapter.paired_text"));
    expect(dialog).toHaveTextContent(t("ui.adapter.paired_keep"));
    fireEvent.click(within(dialog).getByRole("button", { name: t("modal.cancel") }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.some((c) => c.method === "adapter.assign_winusb")).toBe(false);
    // Confirmed: set up as before
    fireEvent.click(screen.getByRole("button", { name: t("ui.setup.auto_button") }));
    dialog = await screen.findByRole("dialog", { name: t("ui.adapter.paired_title") });
    const confirm = within(dialog).getByRole("button", { name: t("ui.setup.auto_button") });
    expect(shielded(confirm)).toBe(true);
    expect(confirm).toHaveClass("btn-danger");
    fireEvent.click(confirm);
    await screen.findByText("The adapter is ready for BlueCaster", {}, { timeout: 5000 });
    expect(calls.some((c) => c.method === "adapter.assign_winusb")).toBe(true);
  });

  it("the setup asks nothing when Windows' list cannot be read (issue #86)", async () => {
    const { calls } = start("#/home", { scenario: "fresh" });
    mock!.adapterDevices = null;
    fireEvent.click(await screen.findByRole("button", { name: "Next" }));
    fireEvent.click(await screen.findByRole("button", { name: t("ui.setup.auto_button") }));
    await screen.findByText("The adapter is ready for BlueCaster", {}, { timeout: 5000 });
    expect(calls.some((c) => c.method === "adapter.windows_devices")).toBe(true);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("the Adapters screen's driver dialog names the devices Windows loses (issue #86)", async () => {
    start("#/adapter", { scenario: "fresh" });
    mock!.adapterDevices = [{ address: "00:11:22:33:44:01", name: "Logitech K380", audio: false }];
    // The fresh scenario opens the setup: leave it
    fireEvent.click(await screen.findByRole("button", { name: t("ui.setup.skip") }));
    location.hash = "#/adapter";
    await screen.findByRole("heading", { level: 1, name: "Adapters" });
    fireEvent.click((await screen.findAllByRole("button", { name: t("ui.adapter.assign_winusb") }))[0]);
    // A warning once the list is in, the devices in place of the usual text
    const dialog = await screen.findByRole("dialog", { name: t("ui.adapter.paired_title") });
    expect(await within(dialog).findByText("Logitech K380")).toBeInTheDocument();
    expect(dialog).toHaveTextContent(t("ui.adapter.paired_receiver"));
    expect(dialog).not.toHaveTextContent(t("ui.adapter.assign_explain"));
    expect(within(dialog).getByRole("button", { name: t("ui.adapter.assign_winusb") })).toHaveClass("btn-danger");
  });

  it("the flyout", async () => {
    start("", { window: "flyout" });
    expect(await screen.findByRole("button", { name: "Connect" })).toBeInTheDocument();
    // Beside Connect, not in a row of its own (issue #86)
    const open = screen.getByRole("button", { name: "Open BlueCaster" });
    expect(open.closest(".fly-actions")).not.toBeNull();
    expect(document.querySelector(".fly-foot")).toBeNull();
  });

  it("the flyout looks like Home's card: the device chooser as its title, the codec in two lines (issue #86)", async () => {
    start("", { window: "flyout" });
    const connect = await screen.findByRole("button", { name: "Connect" });
    // The device once: the chooser in the title's place, no "Device:" row
    const chooser = screen.getByRole("combobox", { name: "Device" });
    expect(chooser.closest(".fly-title")).not.toBeNull();
    expect(screen.queryByText("Device:")).toBeNull();
    expect(document.querySelector(".fly-title .device-name")).toBeNull();
    // Hidden by Escape and on losing the focus: no close button
    expect(screen.queryByRole("button", { name: "Close" })).toBeNull();
    expect(screen.queryByText("—")).toBeNull();
    fireEvent.click(connect);
    await screen.findByRole("button", { name: "Disconnect" });
    // The codec is chosen right there, as on Home (issue #86)
    const codec = await screen.findByRole("combobox", { name: "Codec" });
    expect(codec.closest(".fly-title .codec-name")).not.toBeNull();
    expect(codec).toHaveDisplayValue("LDAC");
    expect(screen.getByRole("status")).toHaveTextContent(/^Connected$/);
    // The link quality beside the codec's details
    const line = document.querySelector(".fly-title .codec-line")!;
    expect(line.querySelector(".codec-badge")).toHaveTextContent("990 kbps");
    expect(line.querySelector(".quality")).not.toBeNull();
    // Then the volume, then Disconnect
    const slider = screen.getByRole("slider", { name: "Volume" });
    const disconnect = screen.getByRole("button", { name: "Disconnect" });
    expect(slider.compareDocumentPosition(disconnect) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("in Japanese", async () => {
    const { core } = start("#/settings");
    await screen.findByRole("heading", { level: 1, name: "Settings" });
    await core.request("settings.set", { settings: { ...core.state.snap!.settings, language: "ja" } });
    expect(await screen.findByRole("link", { name: "ホーム" })).toBeInTheDocument();
  });
});

describe("while the core runs as a service", () => {
  it("names no protected folder, and Quit closes the app instead of the core", async () => {
    const { calls } = start("#/settings", { scenario: "service" });
    await screen.findByRole("heading", { level: 1, name: "Settings" });
    expect(screen.queryByRole("button", { name: t("ui.settings.open_config_folder") })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: t("ui.settings.quit") }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: t("ui.settings.quit") }));
    // app.quit is refused there: app.close follows, and no error shows
    await waitFor(() => expect(calls.map((c) => c.method)).toContain("app.close"));
    expect(calls.map((c) => c.method).filter((m) => m.startsWith("app.q"))).toEqual(["app.quit"]);
    expect(screen.queryByText(t("error.not_in_service"))).toBeNull();
  });

  it("offers no firmware folder and no log folder", async () => {
    start("#/adapter", { scenario: "service" });
    await screen.findByRole("heading", { level: 1, name: "Adapters" });
    expect(screen.queryByRole("button", { name: t("firmware.open_folder") })).toBeNull();
    cleanup();
    mock?.dispose();
    start("#/diagnostics", { scenario: "service" });
    fireEvent.click(await screen.findByRole("tab", { name: "Log" }));
    await screen.findByText(t("ui.diag.clear"));
    expect(screen.queryByRole("button", { name: t("ui.diag.open_logs") })).toBeNull();
  });

  it("keeps the folders outside service mode", async () => {
    start("#/settings");
    expect(await screen.findByRole("button", { name: t("ui.settings.open_config_folder") })).toBeInTheDocument();
    cleanup();
    mock?.dispose();
    start("#/adapter");
    expect((await screen.findAllByRole("button", { name: t("firmware.open_folder") })).length).toBeGreaterThan(0);
    cleanup();
    mock?.dispose();
    start("#/diagnostics");
    fireEvent.click(await screen.findByRole("tab", { name: "Log" }));
    expect(await screen.findByRole("button", { name: t("ui.diag.open_logs") })).toBeInTheDocument();
  });
});

describe("update", () => {
  it("installs at once when nothing streams", async () => {
    start("#/settings", { scenario: "update" });
    fireEvent.click(await screen.findByRole("button", { name: "Update now" }));
    await screen.findByText(/Installing version 2\.0\.0-beta\.1/);
  });

  it("waits for the stream to stop", async () => {
    start("#/settings", { scenario: "update" });
    await mock!.call("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() => expect(mock!["snap"].stream.state).toBe("streaming"));
    fireEvent.click(await screen.findByRole("button", { name: "Update now" }));
    await screen.findByText(/will install when the audio device disconnects/);
    await mock!.call("stream.stop");
    await screen.findByText(/Installing version 2\.0\.0-beta\.1/, {}, { timeout: 3000 });
  });
});
