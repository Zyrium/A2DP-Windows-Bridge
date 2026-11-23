import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../app/App";
import { t } from "../i18n";
import { MockCore, type MockScenario, sampleSnapshot } from "../ipc/mock";
import { createCore } from "../ipc/store";
import {
  adapterIds,
  adapterIdsHint,
  adapterName,
  adapterUseButton,
  chosenMissing,
  driverText,
  isActiveAdapter,
  isChosenAdapter,
} from "./Adapter";
import { chipReportUrl } from "../lib/links";

let mock: MockCore | null = null;

function start(scenario: MockScenario = "normal") {
  location.hash = "#/adapter";
  const calls: { method: string; params: unknown }[] = [];
  mock = new MockCore({ speed: 0, scenario, onCall: (method, params) => calls.push({ method, params }) });
  const core = createCore(mock);
  render(() => <App core={core} />);
  return { calls, mock, core };
}

afterEach(() => {
  cleanup();
  mock?.dispose();
  mock = null;
  vi.restoreAllMocks();
});

describe("AFH", () => {
  it("has its mode and channels beside its text: Auto and Off read only, Manual typed (issue #86)", async () => {
    const { calls, mock } = start();
    const section = (await screen.findByRole("heading", { level: 2, name: "Avoid Wi-Fi channels" })).closest("section")!;
    // The text in a block of its own, the controls in another (issue #86)
    expect(section.querySelector(".afh-text")).toHaveTextContent(t("receiver.afh_tooltip"));
    const head = section.querySelector(".afh-control") as HTMLElement;
    const box = within(head).getByRole("textbox", { name: "Wi-Fi ch" }) as HTMLInputElement;
    // One set of controls only
    expect(within(section).getAllByRole("radiogroup")).toHaveLength(1);
    expect(within(section).getAllByRole("textbox")).toHaveLength(1);
    // Auto: what it avoids now (the mock's stream avoids Wi-Fi channel 6)
    expect(box.readOnly).toBe(true);
    await mock.call("stream.start", { profile: "WH-1000XM5 (LDAC)" });
    await waitFor(() => expect(box.value).toBe("6"), { timeout: 3000 });
    // Off: nothing, read only
    fireEvent.click(within(head).getByRole("radio", { name: "Off" }));
    await waitFor(() => expect(box.value).toBe(""));
    expect(box.readOnly).toBe(true);
    // Manual: typed and saved
    fireEvent.click(within(head).getByRole("radio", { name: "Manual" }));
    await waitFor(() => expect(box.readOnly).toBe(false));
    fireEvent.input(box, { target: { value: "11, 6" } });
    fireEvent.blur(box);
    await waitFor(() =>
      expect(calls.filter((c) => c.method === "settings.set").at(-1)?.params).toMatchObject({ settings: { afh: "6,11" } }),
    );
  });
});

describe("LE Audio switch (issue #86)", () => {
  it("is on Adapters when an adapter supports LE Audio, and saves the setting", async () => {
    const { calls } = start("le");
    const toggle = (await screen.findByRole("switch", { name: t("ui.adapter.le_audio") })) as HTMLInputElement;
    const on = toggle.checked;
    fireEvent.click(toggle);
    await waitFor(() =>
      expect(calls.filter((c) => c.method === "settings.set").at(-1)?.params).toMatchObject({
        settings: { le_audio_experimental: !on },
      }),
    );
  });

  it("is there in developer mode only", async () => {
    const { core } = start("le");
    await screen.findByRole("switch", { name: t("ui.adapter.le_audio") });
    await core.request("settings.set", { settings: { ...core.state.snap!.settings, developer_mode: false } });
    await waitFor(() => expect(screen.queryByText(t("ui.adapter.le_audio"))).toBeNull());
  });

  it("is not there when no adapter supports it", async () => {
    start("le_unsupported");
    await screen.findByRole("heading", { level: 2, name: "Avoid Wi-Fi channels" });
    expect(screen.queryByText(t("ui.adapter.le_audio"))).toBeNull();
  });
});

describe("Adapter firmware", () => {
  it("shows the chip's firmware and saves another chip", async () => {
    const { calls } = start();
    await screen.findByRole("heading", { level: 1, name: "Adapters" });
    await waitFor(() => expect(calls.some((c) => c.method === "adapter.firmware_files")).toBe(true));
    const chip = screen.getByLabelText("Bluetooth Chip") as HTMLSelectElement;
    expect(chip.options[chip.selectedIndex].text).toBe("Auto (RTL8761BU)");
    // In the firmware box of the adapter it applies to, not a section of its own
    expect(chip.closest(".adapter-card")).not.toBeNull();
    const box = screen.getByRole("group", { name: "Firmware: Firmware OK" });
    expect(box).toContainElement(chip);
    // The tag says the files are there; the box can fetch them again
    expect(screen.queryByText("rtl8761bu_config.bin")).toBeNull();
    expect(within(box).getByRole("button", { name: "Download Firmware" })).toBeInTheDocument();

    fireEvent.change(chip, { target: { selectedIndex: 1 } });
    await waitFor(() => expect(calls.some((c) => c.method === "settings.set")).toBe(true));
    const saved = calls.find((c) => c.method === "settings.set")!.params as {
      settings: { bt_chip_pid: number; bt_chip_fw_stem: string };
    };
    expect(saved.settings).toMatchObject({ bt_chip_pid: 0xfffe, bt_chip_fw_stem: "" });
    expect(await screen.findByText(/No Realtek files needed\./)).toBeInTheDocument();
    // The core checks the firmware for the new chip: none needed now
    expect(await screen.findByText("No Realtek files needed")).toBeInTheDocument();
    // The folder is scanned again after the change
    await waitFor(() => expect(calls.filter((c) => c.method === "adapter.firmware_files").length).toBeGreaterThan(1));
  });

  it("loads Realtek's driver firmware first, custom firmware on request and back (issue #86)", async () => {
    const { calls } = start("realtek_driver");
    const box = await screen.findByRole("group", { name: "Firmware: From Realtek's driver" });
    // Nothing to do: no chip choice, no download
    expect(screen.queryByLabelText("Bluetooth Chip")).toBeNull();
    expect(within(box).queryByRole("button", { name: "Download Firmware" })).toBeNull();
    fireEvent.click(within(box).getByRole("button", { name: t("ui.adapter.custom_firmware") }));
    await waitFor(() =>
      expect(calls.filter((c) => c.method === "settings.set").at(-1)?.params).toMatchObject({
        settings: { realtek_custom_firmware: true },
      }),
    );
    // The firmware folder's files as before, and the way back
    const custom = await screen.findByRole("group", { name: "Firmware: Firmware OK" });
    expect(within(custom).getByLabelText("Bluetooth Chip")).toBeInTheDocument();
    expect(within(custom).getByRole("button", { name: "Download Firmware" })).toBeInTheDocument();
    expect(within(custom).queryByRole("button", { name: t("ui.adapter.custom_firmware") })).toBeNull();
    fireEvent.click(within(custom).getByRole("button", { name: t("ui.adapter.driver_firmware") }));
    expect(await screen.findByRole("group", { name: "Firmware: From Realtek's driver" })).toBeInTheDocument();
    expect(calls.filter((c) => c.method === "settings.set").at(-1)?.params).toMatchObject({
      settings: { realtek_custom_firmware: false },
    });
  });

  it("names the adapter as Device Manager does, with its driver and firmware side by side (issue #86)", async () => {
    // The first run's adapters (that scenario opens the setup instead)
    const { mock } = start();
    await screen.findByRole("heading", { level: 1, name: "Adapters" });
    const m = mock as any;
    const [mediatek, realtek] = sampleSnapshot("fresh").adapter.adapters;
    // An Intel adapter on WinUSB that reports no product name
    const intel = {
      ...realtek,
      instance_id: "USB\\VID_8087&PID_0033\\5&1",
      vid: 0x8087,
      pid: 0x0033,
      display_name: "Intel (0x8087:0x0033)",
      vendor: "Intel",
      driver: "winusb",
      realtek_chip: null,
      firmware_present: null,
      vendor_note: { key: "firmware.vendor_intel", args: [] },
      windows_name: null,
      // Neither Windows' Bluetooth nor a stack of BlueCaster ran it yet
      address: null,
      bt_version: null,
    };
    m.snap.adapter = { ...m.snap.adapter, adapters: [mediatek, realtek, intel] };
    m.emit({ kind: "adapter", value: m.snap.adapter });
    const rtl = await screen.findByRole("region", { name: "TP-Link Bluetooth USB Adapter (RTL8761BU)" });
    expect(rtl).toHaveTextContent("00:1A:7D:DA:71:13 | 2357:0604 | v5.1");
    // The driver WinUSB replaces is called the original one (issue #86)
    const rtlDriver = within(rtl).getByRole("group", { name: "Driver: Original driver" });
    const assign = within(rtl).getByRole("button", { name: "Replace with WinUSB driver" });
    expect(rtlDriver).toContainElement(assign);
    expect(assign).toBeEnabled();
    expect(rtlDriver).toHaveTextContent("Replaces the original driver");
    // Windows' Bluetooth runs it: its name, address and version from there
    const mtk = screen.getByRole("region", { name: "MediaTek Bluetooth Adapter" });
    expect(mtk).toHaveTextContent("C0:E3:50:2E:00:09 | 0E8D:7925 | v5.4");
    expect(mtk).not.toHaveTextContent("[unsupported]");
    const fw = within(mtk).getByRole("group", { name: "Firmware: Unsupported" });
    expect(within(fw).getByText("Unsupported")).toHaveClass("st-bad");
    expect(within(fw).getByRole("button", { name: "Download not available" })).toBeDisabled();
    // A vendor's driver is the original one too, its service on hover; BlueCaster
    // cannot use the adapter, so it keeps that driver
    const mtkDriver = within(mtk).getByRole("group", { name: "Driver: Original driver" });
    expect(within(mtkDriver).getByText("Original driver")).toHaveAttribute("title", "mtkbtfilterx");
    expect(within(mtkDriver).getByRole("button", { name: "Replace with WinUSB driver" })).toBeDisabled();
    expect(mtkDriver).toHaveTextContent("BlueCaster cannot use this adapter, so its driver is left unchanged");
    // The chip choice goes with the Realtek adapter only
    expect(within(mtk).queryByLabelText("Bluetooth Chip")).toBeNull();
    expect(within(rtl).getByLabelText("Bluetooth Chip")).toBeInTheDocument();
    // No Experimental tag on any adapter (issue #86)
    const intelCard = screen.getByRole("region", { name: "Intel" });
    expect(intelCard).not.toHaveTextContent(/Experimental/i);
    // Its firmware from the folder, without the Realtek chip choice
    const intelFw = within(intelCard).getByRole("group", { name: "Firmware: From the firmware folder" });
    expect(within(intelFw).queryByLabelText("Bluetooth Chip")).toBeNull();
    expect(intelFw).toHaveTextContent(t("firmware.intel_files"));
    // WinUSB in one name too (issue #86)
    expect(within(intelCard).getByRole("group", { name: "Driver: WinUSB driver" })).toBeInTheDocument();
    // What is known; why the rest is missing on hover
    expect(within(intelCard).getByText("8087:0033")).toHaveAttribute(
      "title",
      "Shown once this adapter has been used.",
    );
    expect(within(rtl).getByText("00:1A:7D:DA:71:13 | 2357:0604 | v5.1")).not.toHaveAttribute("title");
  });

  it("lists the adapters by itself, without a Refresh button (issue #86)", async () => {
    const { calls } = start();
    await screen.findByRole("heading", { level: 1, name: "Adapters" });
    expect(screen.queryByRole("button", { name: "Refresh" })).toBeNull();
    expect(screen.getByRole("button", { name: "Restart Bluetooth" })).toBeInTheDocument();
    // On opening the screen, and when the window gets the focus back
    await waitFor(() => expect(calls.filter((c) => c.method === "adapter.list").length).toBe(1));
    await waitFor(() => expect(calls.some((c) => c.method === "adapter.firmware_files")).toBe(true));
    // Once that request has its answer (one at a time)
    await new Promise((r) => setTimeout(r, 10));
    fireEvent.focus(window);
    await waitFor(() => expect(calls.filter((c) => c.method === "adapter.list").length).toBe(2));
  });

  it("gives the adapter back without a pairing warning", async () => {
    const { mock } = start();
    await screen.findByRole("heading", { level: 1, name: "Adapters" });
    const m = mock as any;
    const saved = m.snap.devices.filter((d: { saved: boolean; le_audio: boolean }) => d.saved && !d.le_audio);
    // Windows paired a saved device too: the restore hands it BlueCaster's key
    m.windowsPaired = [saved[0].address.toLowerCase()];
    fireEvent.click((await screen.findAllByRole("button", { name: "Restore original driver" }))[0]);
    const dialog = await screen.findByRole("dialog", { name: "Return the adapter to Windows" });
    await new Promise((r) => setTimeout(r, 10));
    expect(within(dialog).queryByText(/may not connect in Windows/)).toBeNull();
  });

  it("adapter names", () => {
    const a = { display_name: "RTL8761BU (0x0BDA:0xA729)", vendor: "Realtek", windows_name: null, realtek_chip: "RTL8761BU" };
    // Windows' Bluetooth never ran it: vendor and chip
    expect(adapterName(a)).toBe("Realtek RTL8761BU");
    expect(adapterName({ ...a, display_name: "MediaTek (0x0E8D:0x7925) [unsupported]", vendor: "MediaTek", realtek_chip: null })).toBe(
      "MediaTek",
    );
    expect(adapterName({ ...a, windows_name: "Realtek Bluetooth 5.3 Adapter" })).toBe("Realtek Bluetooth 5.3 Adapter (RTL8761BU)");
    // The chip once only
    expect(adapterName({ ...a, windows_name: "Realtek RTL8761BU Bluetooth" })).toBe("Realtek RTL8761BU Bluetooth");
    expect(adapterIds({ address: null, vid: 0x0bda, pid: 0xa729, bt_version: null })).toBe("0BDA:A729");
    // A Realtek adapter's USB serial gives the address before any stack ran it
    expect(adapterIds({ address: "00:E0:4C:23:99:87", vid: 0x0bda, pid: 0xa729, bt_version: null })).toBe(
      "00:E0:4C:23:99:87 | 0BDA:A729",
    );
    expect(adapterIdsHint({ address: "00:E0:4C:23:99:87", bt_version: null })).toMatch(/Shown once this adapter has been used/);
    expect(adapterIdsHint({ address: "00:E0:4C:23:99:87", bt_version: "5.1" })).toBeUndefined();
    // The driver in two names only, whatever the manufacturer
    expect(driverText("bluetooth").text).toBe("Original driver");
    expect(driverText("other").text).toBe("Original driver");
    expect(driverText("winusb").text).toBe("WinUSB driver");
    expect(driverText("none").text).toBe("No driver");
    // An Intel adapter on WinUSB without a product string
    const intel = { ...a, display_name: "Intel (0x8087:0x0033)", vendor: "Intel", realtek_chip: null };
    expect(adapterName(intel)).toBe("Intel");
  });

  it("opens the firmware folder", async () => {
    const { mock } = start();
    const open = vi.spyOn(mock, "openFolder");
    fireEvent.click(await screen.findByRole("button", { name: "Open Firmware Folder" }));
    await waitFor(() => expect(open).toHaveBeenCalledWith("C:\\Users\\you\\AppData\\Roaming\\BlueCaster\\firmware"));
  });

  it("downloads the missing firmware after the user has read the terms", async () => {
    const { calls } = start();
    // A chip whose files are not in the sample folder
    const chip = (await screen.findByLabelText("Bluetooth Chip")) as HTMLSelectElement;
    fireEvent.change(chip, { target: { value: [...chip.options].find((o) => o.text.startsWith("RTL8852BU"))!.value } });
    expect(await screen.findByText("Firmware not found")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Download Firmware" }));
    const dialog = await screen.findByRole("dialog", { name: "Download the Realtek firmware" });
    expect(dialog).toHaveTextContent("LICENCE.rtlwifi_firmware.txt");
    expect(calls.some((c) => c.method === "adapter.download_firmware")).toBe(false);
    fireEvent.click([...dialog.querySelectorAll("button")].find((b) => b.textContent === "Download Firmware")!);
    await waitFor(() =>
      expect(calls.find((c) => c.method === "adapter.download_firmware")?.params).toEqual({ file: "rtl8852bu_fw.bin" }),
    );
    expect(await screen.findByText("Firmware downloaded. It takes effect the next time you connect.")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText("Firmware not found")).not.toBeInTheDocument());
  });

  /** Opens the download dialog for a chip with no files and starts the
   *  download, which the test answers itself */
  async function startDownload() {
    const { mock } = start();
    let answer!: { resolve: (v: unknown) => void; reject: (e: unknown) => void };
    vi.spyOn(mock as any, "downloadFirmware").mockImplementation(
      () => new Promise((resolve, reject) => (answer = { resolve, reject })),
    );
    const chip = (await screen.findByLabelText("Bluetooth Chip")) as HTMLSelectElement;
    fireEvent.change(chip, { target: { value: [...chip.options].find((o) => o.text.startsWith("RTL8852BU"))!.value } });
    expect(await screen.findByText("Firmware not found")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Download Firmware" }));
    const dialog = await screen.findByRole("dialog", { name: "Download the Realtek firmware" });
    fireEvent.click([...dialog.querySelectorAll("button")].find((b) => b.textContent === "Download Firmware")!);
    await waitFor(() => expect(answer).toBeDefined());
    return { dialog, answer: () => answer };
  }

  it("shows a spinner and stays open while downloading", async () => {
    const { dialog, answer } = await startDownload();
    expect(await within(dialog).findByRole("status")).toHaveTextContent("Downloading the firmware from kernel.org");
    expect(dialog.querySelector(".spinner.big")).not.toBeNull();
    // Neither Cancel, Escape nor the backdrop closes it meanwhile
    const cancel = within(dialog).getByRole("button", { name: "Cancel" });
    expect(cancel).toBeDisabled();
    fireEvent.keyDown(dialog, { key: "Escape" });
    fireEvent.click(dialog.parentElement!);
    expect(screen.getByRole("dialog", { name: "Download the Realtek firmware" })).toBeInTheDocument();
    answer().resolve([]);
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("does not call the UI's own timeout a failed download", async () => {
    const { answer } = await startDownload();
    answer().reject({ code: "ipc.timeout", message: { key: "ipc.error.timeout", args: ["adapter.download_firmware"] } });
    expect(await screen.findByText(/The download is taking a while/)).toBeInTheDocument();
    expect(screen.queryByText(/not responding/i)).not.toBeInTheDocument();
  });
});

describe("Adapter to use", () => {
  const winusb = (instance_id: string, vid: number, pid: number) =>
    ({ ...sampleSnapshot().adapter.adapters[0]!, instance_id, vid, pid, driver: "winusb" }) as const;

  it("marks the adapter in use and offers the ones on WinUSB", () => {
    const rtl = winusb("USB\\VID_2357&PID_0604\\1", 0x2357, 0x0604);
    const intel = winusb("USB\\VID_8087&PID_0033\\2", 0x8087, 0x0033);
    const active = { display_name: "", vid: 0x8087, pid: 0x0033, vendor: "Intel" };
    expect(isActiveAdapter(intel, active)).toBe(true);
    expect(isActiveAdapter(rtl, active)).toBe(false);
    expect(isActiveAdapter(intel, null)).toBe(false);
    // The same VID:PID with Windows' driver is not the one open
    expect(isActiveAdapter({ ...intel, driver: "bluetooth" }, active)).toBe(false);
    expect(isChosenAdapter(intel, "usb\\vid_8087&pid_0033\\2")).toBe(true);
    expect(isChosenAdapter(intel, "")).toBe(false);
    // Nothing chosen: "use" on all on WinUSB but the one in use
    expect(adapterUseButton(intel, "", active)).toBeNull();
    expect(adapterUseButton(rtl, "", active)).toBe("use");
    expect(adapterUseButton({ ...rtl, driver: "bluetooth" }, "", active)).toBeNull();
    // One chosen: "auto" on it, "use" on the others
    expect(adapterUseButton(rtl, rtl.instance_id, active)).toBe("auto");
    expect(adapterUseButton(intel, rtl.instance_id, active)).toBe("use");
    // A chosen adapter that is not on WinUSB is told, with a way back
    expect(chosenMissing([rtl], intel.instance_id)).toBe(true);
    expect(chosenMissing([rtl, intel], intel.instance_id)).toBe(false);
    expect(chosenMissing([rtl], "")).toBe(false);
  });

  it("chooses an adapter, restarts Bluetooth on it and shows it in use", async () => {
    const { calls, mock } = start("adapters");
    await screen.findByRole("heading", { level: 1 });
    const card = (name: RegExp) => screen.getByRole("region", { name }) as HTMLElement;
    const useButton = (name: RegExp) => within(card(name)).queryByRole("button", { name: t("ui.adapter.use") });
    // The stack runs on the first one found: the Realtek adapter
    await mock.call("adapter.restart");
    (mock as unknown as { setAdapter(s: string): void }).setAdapter("ready");
    await waitFor(() => expect(card(/TP-Link/)).toHaveClass("adapter-active"));
    expect(within(card(/TP-Link/)).getByText(t("ui.adapter.active"))).toBeInTheDocument();
    // The one in use has no button while none is chosen; the other has
    expect(useButton(/TP-Link/)).toBeNull();
    // The Intel card chosen: saved, Bluetooth stops, the card says chosen
    fireEvent.click(useButton(/Intel/)!);
    await waitFor(() =>
      expect(calls.filter((c) => c.method === "settings.set").at(-1)?.params).toMatchObject({
        settings: { adapter_id: "USB\\VID_8087&PID_0033\\5&19A12CFC&0&14" },
      }),
    );
    await waitFor(() => expect(within(card(/Intel/)).getByText(t("ui.adapter.chosen"))).toBeInTheDocument());
    expect(card(/TP-Link/)).not.toHaveClass("adapter-active");
    // Back to automatic on the chosen one, use on the other
    expect(within(card(/Intel/)).getByRole("button", { name: t("ui.adapter.use_auto") })).toBeInTheDocument();
    expect(useButton(/TP-Link/)).not.toBeNull();
    // Started again: the chosen one is in use
    (mock as unknown as { setAdapter(s: string): void }).setAdapter("ready");
    await waitFor(() => expect(card(/Intel/)).toHaveClass("adapter-active"));
    expect(within(card(/Intel/)).queryByText(t("ui.adapter.chosen"))).toBeNull();
    expect(screen.getByText(t("ui.adapter.active_line", "Intel(R) Wireless Bluetooth(R)"), { exact: false })).toBeInTheDocument();
    // Automatic again
    fireEvent.click(within(card(/Intel/)).getByRole("button", { name: t("ui.adapter.use_auto") }));
    await waitFor(() =>
      expect(calls.filter((c) => c.method === "settings.set").at(-1)?.params).toMatchObject({
        settings: { adapter_id: "" },
      }),
    );
  });

  it("is not offered with one adapter", async () => {
    start();
    await screen.findByRole("heading", { level: 1 });
    expect(screen.queryByRole("button", { name: t("ui.adapter.use") })).toBeNull();
  });
});

describe("Chip probe (developer mode)", () => {
  it("offers WinUSB for a chip BlueCaster cannot use and reads a WinUSB adapter's chip", async () => {
    const { calls, mock } = start();
    await screen.findByRole("heading", { level: 1, name: "Adapters" });
    const m = mock as any;
    const [mediatek, realtek] = sampleSnapshot("fresh").adapter.adapters;
    m.snap.adapter = { ...m.snap.adapter, adapters: [mediatek, { ...realtek, driver: "winusb" }] };
    m.emit({ kind: "adapter", value: m.snap.adapter });
    const mtk = await screen.findByRole("region", { name: "MediaTek Bluetooth Adapter" });
    const assign = within(mtk).getByRole("button", { name: "Replace with WinUSB driver" });
    // Developer mode off: neither
    expect(assign).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Read chip info" })).toBeNull();

    await mock.call("settings.set", { settings: { ...m.snap.settings, developer_mode: true } });
    await waitFor(() => expect(assign).toBeEnabled());
    expect(within(mtk).getByRole("group", { name: "Driver: Original driver" })).toHaveTextContent(
      "Replaces the original driver",
    );
    // The probe on an adapter with WinUSB only
    expect(within(mtk).queryByRole("button", { name: "Read chip info" })).toBeNull();
    const rtl = screen.getByRole("region", { name: "TP-Link Bluetooth USB Adapter (RTL8761BU)" });
    const reveal = vi.spyOn(mock, "revealPath");
    fireEvent.click(within(rtl).getByRole("button", { name: "Read chip info" }));
    expect(await screen.findByText(/^Chip info saved: Intel, Bluetooth 5\.3/)).toBeInTheDocument();
    expect(calls.find((c) => c.method === "adapter.probe")?.params).toEqual({ instance_id: realtek.instance_id });
    expect(reveal).toHaveBeenCalledWith(expect.stringMatching(/probe-\d{8}_\d{6}\.json$/));
    // The summary stays on the card, with a prefilled chip report
    expect(within(rtl).getByText(/^Intel, Bluetooth 5\.3/)).toBeInTheDocument();
    const open = vi.spyOn(mock, "openUrl");
    reveal.mockClear();
    fireEvent.click(within(rtl).getByRole("button", { name: "Report on GitHub" }));
    await waitFor(() => expect(open).toHaveBeenCalled());
    const url = new URL(open.mock.calls[0][0] as string);
    expect(url.searchParams.get("template")).toBe("chip_report.yml");
    expect(url.searchParams.get("chip")).toMatch(/^Intel, Bluetooth 5\.3/);
    expect(url.searchParams.get("title")).toBe("Chip support: Intel");
    // The report itself is in the issue: nothing to drag
    expect(url.searchParams.get("probe")).toMatch(/^```json\n\{[\s\S]*"Read Local Version Information"[\s\S]*\}\n```$/);
    expect(reveal).not.toHaveBeenCalled();
  });
});

describe("chipReportUrl", () => {
  const base = { version: "3.0.2", adapter: "Barrot (0x33FA:0x0012)", summary: "Barrot, USB 33FA:0012", info: "" };
  it("puts the report in the issue when it fits", () => {
    const { url, attach } = chipReportUrl({ ...base, report: JSON.stringify({ steps: [{ answer: "0e0c" }] }) });
    expect(attach).toBe(false);
    expect(new URL(url).searchParams.get("probe")).toBe('```json\n{\n "steps": [\n  {\n   "answer": "0e0c"\n  }\n ]\n}\n```');
  });
  it("falls back to one line, then to an empty field the user must fill", () => {
    const steps = (n: number) => JSON.stringify({ steps: Array.from({ length: n }, () => ({ a: "0e" })) });
    // Too long indented, short enough on one line
    const mid = chipReportUrl({ ...base, report: steps(250) });
    expect(mid.attach).toBe(false);
    expect(new URL(mid.url).searchParams.get("probe")).toBe(`\`\`\`json\n${steps(250)}\n\`\`\``);
    // Too long either way: GitHub refuses the issue until the file is attached
    const big = chipReportUrl({ ...base, report: JSON.stringify({ x: "0".repeat(8000) }) });
    expect(big.attach).toBe(true);
    expect(new URL(big.url).searchParams.has("probe")).toBe(false);
    expect(chipReportUrl({ ...base, report: "" }).attach).toBe(true);
  });
});

describe("Firmware loader progress", () => {
  it("shows a loader while the adapter starts, its failure, and its steps in developer mode", async () => {
    const { mock } = start();
    await screen.findByRole("heading", { level: 1, name: "Adapters" });
    const m = mock as any;
    const show = (loader: unknown) => {
      m.snap.adapter = { ...m.snap.adapter, loader };
      m.emit({ kind: "adapter", value: m.snap.adapter });
    };
    show({ vendor: "Intel", step: "Secure Send", percent: 42 });
    const line = await screen.findByText(/^Intel firmware: loading/);
    expect(line).toHaveTextContent("Intel firmware: loading 42%");
    expect(line).not.toHaveTextContent("Secure Send");
    // Ready: only in developer mode, with the step
    show({ vendor: "Intel", step: "firmware ibt-0040-0041.sfi loaded", ok: true });
    await waitFor(() => expect(screen.queryByText(/^Intel firmware/)).toBeNull());
    await mock.call("settings.set", { settings: { ...m.snap.settings, developer_mode: true } });
    expect(await screen.findByText(/^Intel firmware: ready/)).toHaveTextContent("(firmware ibt-0040-0041.sfi loaded)");
    show({ vendor: "Intel", step: "no answer from the controller (Read Version)", ok: false });
    expect(await screen.findByText(/^Intel firmware could not be loaded/)).toHaveTextContent("(no answer");
  });
});

describe("Hung adapter (issue #86)", () => {
  it("asks for a power cycle instead of Zadig or the WinUSB driver", async () => {
    const { mock } = start();
    await screen.findByRole("heading", { level: 1, name: "Adapters" });
    mock.setHung();
    expect(await screen.findByText(t("ui.adapter.hung_title"))).toBeInTheDocument();
    expect(screen.queryByText(t("firmware.no_adapters"))).toBeNull();
    expect(screen.queryByText(t("firmware.auto_no_adapter"))).toBeNull();
    expect(screen.queryByText(t("error.btstack_init"))).toBeNull();
  });
});
