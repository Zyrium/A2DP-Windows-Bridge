import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../app/App";
import { t } from "../i18n";
import { MockCore } from "../ipc/mock";
import { createCore } from "../ipc/store";

let mock: MockCore | null = null;

afterEach(() => {
  cleanup();
  mock?.dispose();
  mock = null;
});

async function open(scenario?: "adapters") {
  location.hash = "#/developer";
  const calls: { method: string; params: unknown }[] = [];
  mock = new MockCore({
    speed: 0,
    scenario,
    onCall: (method, params) => calls.push({ method, params }),
  });
  const core = createCore(mock);
  render(() => <App core={core} />);
  await screen.findByRole("navigation");
  return { core, calls };
}

describe("developer screen", () => {
  it("sets each vendor's protocol and options by vendor id (issue #237)", async () => {
    const { core, calls } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(
      await screen.findByRole("tab", { name: t("ui.dev.vendors") }),
    );
    // One section a vendor the core lists, OPPO with its own option
    expect(
      await screen.findByRole("heading", { name: "Huawei" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "OPPO" })).toBeInTheDocument();
    expect(screen.getAllByLabelText(t("ui.dev.vendor_mode"))).toHaveLength(2);
    const sent = () =>
      (
        calls.filter((c) => c.method === "settings.set").pop()!.params as {
          settings: { dev_vendors: unknown };
        }
      ).settings.dev_vendors;

    fireEvent.change(screen.getAllByLabelText(t("ui.dev.vendor_mode"))[1], {
      target: { value: "always" },
    });
    await screen.findByText(t("ui.dev.active"));
    expect(sent()).toEqual({ oppo: { mode: "always", options: [] } });
    fireEvent.click(
      screen.getByRole("switch", {
        name: t("ui.dev.vendor_option.oppo.every_connection"),
      }),
    );
    await waitFor(() =>
      expect(sent()).toEqual({
        oppo: { mode: "always", options: ["every_connection"] },
      }),
    );

    fireEvent.click(screen.getByRole("button", { name: t("ui.dev.reset") }));
    await screen.findByText(t("ui.dev.vendors_intro"));
    expect(sent()).toEqual({});
  });

  it("keeps the MediaTek experiments on the Debug tab, off by default (issue #86)", async () => {
    const { core, calls } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(
      await screen.findByRole("tab", { name: t("ui.dev.debug") }),
    );
    await screen.findByText(t("ui.dev.debug_intro"));
    const pad = screen.getByLabelText(
      t("ui.dev.mtk_iso_pad"),
    ) as HTMLInputElement;
    // The interrupt interface is the default: padding counts until the
    // bulk endpoint is chosen
    expect(pad).toBeEnabled();
    expect(pad.placeholder).toBe("342");
    fireEvent.click(
      screen.getByRole("switch", { name: t("ui.dev.mtk_iso_bulk") }),
    );
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_debug.mtk_iso_bulk).toBe(true),
    );
    await screen.findByText(t("ui.dev.active"));
    await waitFor(() => expect(pad).toBeDisabled());
    fireEvent.click(
      screen.getByRole("switch", { name: t("ui.dev.mtk_iso_bulk") }),
    );
    await waitFor(() => expect(pad).toBeEnabled());
    fireEvent.change(pad, { target: { value: "99999" } });
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_debug.mtk_iso_pad).toBe(4096),
    );
    fireEvent.change(screen.getByLabelText(t("ui.dev.mtk_command")), {
      target: { value: " fc00 01 " },
    });
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_debug.mtk_command).toBe("FC00 01"),
    );
    fireEvent.click(
      screen.getByRole("switch", {
        name: t("ui.dev.mtk_command_before_reset"),
      }),
    );
    await waitFor(() =>
      expect(
        core.state.snap!.settings.dev_debug.mtk_command_before_reset,
      ).toBe(true),
    );
    fireEvent.change(screen.getByLabelText(t("ui.dev.mtk_register")), {
      target: { value: " 40 01 0000abcd 00000001 " },
    });
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_debug.mtk_register).toBe(
        "40 01 0000ABCD 00000001",
      ),
    );
    // LE Audio: empty fields are Auto, RTN 0 is a value of its own
    const latency = screen.getByLabelText(t("ui.dev.le_latency_p_to_c")) as HTMLInputElement;
    expect(latency.placeholder).toBe(t("codec.auto"));
    fireEvent.change(latency, { target: { value: "100" } });
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_debug.le_latency_p_to_c_ms).toBe(100),
    );
    const rtn = screen.getByLabelText(t("ui.dev.le_rtn_p_to_c"));
    fireEvent.change(rtn, { target: { value: "0" } });
    await waitFor(() => expect(core.state.snap!.settings.dev_debug.le_rtn_p_to_c).toBe(0));
    fireEvent.change(rtn, { target: { value: "" } });
    await waitFor(() => expect(core.state.snap!.settings.dev_debug.le_rtn_p_to_c).toBeNull());
    fireEvent.change(screen.getByLabelText(t("ui.dev.le_mic_preset")), {
      target: { value: "32_2_1" },
    });
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_debug.le_mic_preset).toBe("32_2_1"),
    );
    fireEvent.click(
      screen.getByRole("switch", { name: t("ui.dev.le_mic_retry_same_link") }),
    );
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_debug.le_mic_retry_same_link).toBe(true),
    );
    fireEvent.click(
      screen.getByRole("switch", { name: t("ui.dev.le_keys_to_windows") }),
    );
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_debug.le_keys_to_windows).toBe(true),
    );
    // The comparison shows numbers only
    fireEvent.click(
      screen.getByRole("button", { name: t("ui.dev.keys_compare") }),
    );
    await screen.findByText(t("ui.dev.keys_link", 1, 0, 0, 0));
    expect(screen.getByText(t("ui.dev.keys_le", 0, 1, 0, 0))).toBeInTheDocument();
    // What Windows keeps: names, types and sizes, never the data
    expect(
      screen.getByText(
        t("ui.dev.keys_le_values", "LTK (binary, 16 B), ERand (QWORD, 8 B)"),
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(t("ui.dev.keys_radio_values", "CentralIRK (binary, 16 B)")),
    ).toBeInTheDocument();
    expect(calls.find((c) => c.method === "dev.compare_windows_keys")).toBeTruthy();

    // "Reset all" on the app tab clears them too
    fireEvent.click(
      screen.getByRole("tab", { name: t("ui.dev.app_settings") }),
    );
    fireEvent.click(
      await screen.findByRole("button", { name: t("ui.dev.reset_all") }),
    );
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_debug).toEqual({
        mtk_iso_bulk: false,
        mtk_iso_pad: 0,
        mtk_command: "",
        mtk_iso_queue: 0,
        mtk_command_before_reset: false,
        mtk_register: "",
        le_keys_to_windows: false,
        le_latency_c_to_p_ms: 0,
        le_latency_p_to_c_ms: 0,
        le_rtn_c_to_p: null,
        le_rtn_p_to_c: null,
        le_interleaved: false,
        le_mic_preset: "",
        le_mic_retry_same_link: false,
      }),
    );
  });

  it("is hidden until developer mode is on", async () => {
    const { core } = await open();
    expect(screen.queryByRole("link", { name: "Developer" })).toBeNull();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    expect(
      await screen.findByRole("link", { name: "Developer" }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("heading", { level: 1, name: "Developer" }),
    ).toBeInTheDocument();
  });

  it("saves the overrides and resets them from the app settings", async () => {
    const { core, calls } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(
      await screen.findByRole("tab", { name: t("ui.dev.codec_settings") }),
    );
    fireEvent.click(
      await screen.findByRole("switch", {
        name: new RegExp(t("ui.dev.rtp_marker")),
      }),
    );
    await screen.findByText(t("ui.dev.active"));
    const sent = calls.filter((c) => c.method === "settings.set").pop()!
      .params as { settings: { dev_overrides: unknown } };
    expect(sent.settings.dev_overrides).toMatchObject({
      rtp_marker: true,
      aac_bitrate_kbps: 0,
    });

    const bitrate = screen.getByLabelText(
      t("ui.dev.aac_bitrate"),
    ) as HTMLInputElement;
    fireEvent.input(bitrate, { target: { value: "9999" } });
    fireEvent.change(bitrate, { target: { value: "9999" } });
    await screen.findByDisplayValue("512");

    // The send queue limit is clamped the same way; 0 means the default
    const queueWait = screen.getByLabelText(
      t("ui.dev.queue_wait"),
    ) as HTMLInputElement;
    fireEvent.input(queueWait, { target: { value: "5" } });
    fireEvent.change(queueWait, { target: { value: "5" } });
    await screen.findByDisplayValue("50");
    expect(core.state.snap!.settings.dev_overrides.queue_max_wait_ms).toBe(50);
    fireEvent.change(queueWait, { target: { value: "" } });
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_overrides.queue_max_wait_ms).toBe(0),
    );

    // One reset for the whole screen, in the app settings
    expect(
      screen.queryByRole("button", { name: t("ui.dev.reset") }),
    ).toBeNull();
    fireEvent.click(
      screen.getByRole("tab", { name: t("ui.dev.app_settings") }),
    );
    const reset = await screen.findByRole("button", {
      name: t("ui.dev.reset_all"),
    });
    expect(reset).toBeEnabled();
    fireEvent.click(reset);
    await waitFor(() => expect(reset).toBeDisabled());
    expect(core.state.snap!.settings.dev_overrides.rtp_marker).toBe(false);
  });

  it("saves a codec override with the others", async () => {
    const { core } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(
      await screen.findByRole("tab", { name: t("ui.dev.codec_settings") }),
    );
    fireEvent.click(
      await screen.findByRole("switch", {
        name: new RegExp(t("ui.dev.auto_codec_settings")),
      }),
    );
    await screen.findByText(t("ui.dev.active"));
    expect(core.state.snap!.settings.dev_overrides.auto_codec_settings).toBe(
      true,
    );
  });

  it("keeps the Volume Mixer switch in the app settings (issue #86)", async () => {
    const { core } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    expect(
      await screen.findByRole("tab", { name: t("ui.dev.app_settings") }),
    ).toHaveAttribute("aria-selected", "true");
    fireEvent.click(
      await screen.findByRole("switch", {
        name: new RegExp(t("ui.dev.volume_mixer")),
      }),
    );
    await waitFor(() =>
      expect(core.state.snap!.settings.volume_mixer).toBe(true),
    );
    expect(
      await screen.findByRole("link", { name: t("ui.nav.mixer") }),
    ).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("tab", { name: t("ui.dev.codec_settings") }),
    );
    expect(
      screen.queryByRole("switch", {
        name: new RegExp(t("ui.dev.volume_mixer")),
      }),
    ).toBeNull();
  });

  it("saves the alpha update channel in the app settings", async () => {
    const { core } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(
      await screen.findByRole("switch", { name: t("ui.dev.include_alphas") }),
    );
    await waitFor(() =>
      expect(core.state.snap!.settings.include_alphas).toBe(true),
    );
  });

  it("saves the logging settings in range and puts markers in the log", async () => {
    const { core, calls } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(
      await screen.findByRole("tab", { name: t("ui.dev.logging") }),
    );

    const keep = (await screen.findByLabelText(
      t("ui.dev.log_keep_days"),
    )) as HTMLInputElement;
    fireEvent.change(keep, { target: { value: "1000" } });
    await screen.findByDisplayValue("365");
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_log.keep_days).toBe(365),
    );

    fireEvent.change(screen.getByLabelText(t("ui.dev.log_always")), {
      target: { value: "trace" },
    });
    await screen.findByText(t("ui.dev.log_trace_warn"));
    fireEvent.click(
      screen.getByRole("switch", { name: new RegExp(t("ui.dev.log_health")) }),
    );
    await screen.findByRole("switch", {
      name: new RegExp(t("ui.dev.log_health")),
      checked: true,
    });
    const sent = calls.filter((c) => c.method === "settings.set").pop()!
      .params as { settings: { dev_log: unknown } };
    expect(sent.settings.dev_log).toMatchObject({
      keep_days: 365,
      always_verbose: "trace",
      health_every_interval: true,
    });

    fireEvent.input(screen.getByLabelText(t("ui.dev.mark_note")), {
      target: { value: "stutter" },
    });
    fireEvent.click(screen.getByRole("button", { name: t("ui.dev.mark_add") }));
    await screen.findByText(t("ui.dev.mark_done", 1));
    expect(
      calls.some(
        (c) =>
          c.method === "diag.mark" &&
          (c.params as { note: string }).note === "stutter",
      ),
    ).toBe(true);

    fireEvent.click(
      screen.getByRole("button", { name: t("ui.dev.log_reset") }),
    );
    await screen.findByDisplayValue("7");
    await waitFor(() =>
      expect(core.state.snap!.settings.dev_log.always_verbose).toBeNull(),
    );
  });
});

describe("chip tools", () => {
  it("runs a read-only script, and asks before lines that may change the chip", async () => {
    const { core, calls } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(await screen.findByRole("tab", { name: "Chip" }));
    const box = (await screen.findByRole("textbox", {
      name: "Chip script",
    })) as HTMLTextAreaElement;
    expect(box.value).toContain("hci 1001");
    const run = screen.getByRole("button", { name: "Run" });
    // The example only reads: it runs at once
    await waitFor(() => expect(run).toBeEnabled());
    fireEvent.click(run);
    expect(await screen.findByText("2 of 2 lines done")).toBeInTheDocument();
    const sent = calls.find((c) => c.method === "adapter.run_script")!
      .params as { allow_writes: boolean };
    expect(sent.allow_writes).toBe(false);
    expect(screen.getByLabelText("Result")).toHaveTextContent(
      "line 2: hci 1001",
    );

    // A vendor command: listed, and run only when allowed
    fireEvent.input(box, { target: { value: "hci fc05 ff\n" } });
    expect(
      await screen.findByText("These lines may change the chip"),
    ).toBeInTheDocument();
    expect(screen.getByText("line 1: hci fc05 ff")).toBeInTheDocument();
    expect(run).toBeDisabled();
    fireEvent.click(screen.getByRole("switch", { name: "Run them" }));
    expect(run).toBeEnabled();
    fireEvent.click(run);
    await waitFor(() =>
      expect(
        calls.filter((c) => c.method === "adapter.run_script").pop()!.params,
      ).toMatchObject({ allow_writes: true }),
    );

    // Errors and denied lines never run
    fireEvent.input(box, { target: { value: "fly away\n" } });
    expect(
      await screen.findByText("The script has errors"),
    ).toBeInTheDocument();
    expect(run).toBeDisabled();
    fireEvent.input(box, { target: { value: "hci fc00 01\n" } });
    expect(
      await screen.findByText(
        "These lines could permanently change the chip and will not run",
      ),
    ).toBeInTheDocument();
    expect(run).toBeDisabled();
  });
});

describe("Auracast tab", () => {
  it("takes a matrix's rates from the front and rear rows, not Quality", async () => {
    const { core, calls } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(
      await screen.findByRole("tab", { name: t("ui.dev.auracast") }),
    );
    const start = await screen.findByRole("button", {
      name: t("receiver.start"),
    });
    // Quality at 24 kHz, then the matrix: front 48 kHz (its own), rear 24 kHz
    fireEvent.click(
      screen.getByRole("radio", { name: t("ui.dev.auracast_sq") }),
    );
    fireEvent.click(
      screen.getByRole("radio", { name: t("ui.dev.auracast_matrix") }),
    );
    const rates = await screen.findAllByRole("radio", {
      name: t("ui.dev.auracast_24"),
    });
    expect(rates).toHaveLength(2);
    fireEvent.click(rates[1]!);
    expect(
      screen.getByRole("radio", { name: t("ui.dev.auracast_sq") }),
    ).toBeDisabled();
    fireEvent.click(start);
    await screen.findByText(t("status.broadcasting"));
    expect(
      calls.find((c) => c.method === "stream.start_broadcast")!.params,
    ).toMatchObject({ quality: "hq", rear_quality: "sq", layout: "matrix_stereo_stereo" });
  });

  it("chooses the adapter for broadcasts, by name and MAC address", async () => {
    const { core, calls } = await open("adapters");
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(await screen.findByRole("tab", { name: t("ui.dev.auracast") }));
    const select = (await screen.findByLabelText(t("ui.dev.auracast_adapter"))) as HTMLSelectElement;
    const labels = [...select.options].map((o) => o.textContent);
    // The current adapter first, then each one with its address
    expect(labels[0]).toBe(t("ui.dev.auracast_adapter_current"));
    expect(labels.slice(1)).toEqual([
      expect.stringContaining("00:1A:7D:DA:71:13"),
      expect.stringContaining("F4:26:79:00:00:01"),
    ]);
    expect(select.value).toBe("");
    fireEvent.change(select, { target: { selectedIndex: 2 } });
    await waitFor(() =>
      expect(calls.filter((c) => c.method === "settings.set").at(-1)?.params).toMatchObject({
        settings: { auracast_adapter_id: "USB\\VID_8087&PID_0033\\5&19A12CFC&0&14" },
      }),
    );
  });

  it("offers no adapter choice with one adapter", async () => {
    const { core } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(await screen.findByRole("tab", { name: t("ui.dev.auracast") }));
    await screen.findByRole("button", { name: t("receiver.start") });
    expect(screen.queryByLabelText(t("ui.dev.auracast_adapter"))).toBeNull();
  });

  it("starts and stops a broadcast", async () => {
    const { core, calls } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(
      await screen.findByRole("tab", { name: t("ui.dev.auracast") }),
    );
    const start = await screen.findByRole("button", {
      name: t("receiver.start"),
    });

    // Name and password are checked in bytes
    const name = screen.getByLabelText(t("ui.dev.auracast_name"));
    fireEvent.input(name, { target: { value: "abc" } });
    expect(
      await screen.findByText(t("ui.dev.auracast_err_length", 4, 32)),
    ).toBeInTheDocument();
    expect(start).toBeDisabled();
    fireEvent.input(name, { target: { value: "Living room" } });
    const code = screen.getByLabelText(t("ui.dev.auracast_code"));
    fireEvent.input(code, { target: { value: "ああああああ" } });
    expect(
      await screen.findByText(t("ui.dev.auracast_err_length", 4, 16)),
    ).toBeInTheDocument();
    fireEvent.input(code, { target: { value: "secret" } });
    await waitFor(() => expect(start).toBeEnabled());
    fireEvent.click(
      screen.getByRole("radio", { name: t("ui.dev.auracast_sq") }),
    );
    fireEvent.click(
      screen.getByRole("radio", { name: t("ui.dev.auracast_mono") }),
    );

    fireEvent.click(start);
    expect(
      await screen.findByText(t("status.broadcasting")),
    ).toBeInTheDocument();
    expect(
      calls.find((c) => c.method === "stream.start_broadcast")!.params,
    ).toEqual({
      name: "Living room",
      code: "secret",
      quality: "sq",
      rear_quality: null,
      mono: true,
      layout: null,
      source: "tone",
    });
    expect(screen.getByText(/1 ch over 1 BIS, 24_2_2/)).toBeInTheDocument();
    expect(name).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: t("receiver.stop") }));
    expect(
      await screen.findByRole("button", { name: t("receiver.start") }),
    ).toBeEnabled();
    expect(calls.some((c) => c.method === "stream.stop")).toBe(true);
    expect(screen.queryByText(t("status.broadcasting"))).toBeNull();
  });

  it("keeps a broadcast whose adapter stopped working while it starts again", async () => {
    const { core } = await open();
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(
      await screen.findByRole("tab", { name: t("ui.dev.auracast") }),
    );
    fireEvent.click(
      await screen.findByRole("button", { name: t("receiver.start") }),
    );
    await screen.findByText(t("status.broadcasting"));

    // The core's state while it sets the broadcast up on a new stack
    const fake = mock as unknown as {
      setStream(state: string, key: string, args?: string[]): void;
    };
    fake.setStream("reconnecting", "status.reconnect_attempt", ["1", "5"]);
    expect(
      await screen.findByText(t("status.reconnect_attempt", "1", "5")),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: t("receiver.stop") }),
    ).toBeEnabled();
    expect(screen.getByLabelText(t("ui.dev.auracast_name"))).toBeDisabled();

    fake.setStream("streaming", "status.broadcasting");
    expect(
      await screen.findByText(t("status.broadcasting")),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: t("receiver.stop") }),
    ).toBeEnabled();
  });

  it("shows why a broadcast did not start", async () => {
    const { core } = await open();
    // The core refuses a broadcast while developer mode is off
    await expect(
      core.request("stream.start_broadcast", {
        name: "BlueCaster",
        quality: "hq",
        source: "tone",
      }),
    ).rejects.toMatchObject({
      code: "developer.off",
    });
    await core.request("settings.set", {
      settings: { ...core.state.snap!.settings, developer_mode: true },
    });
    fireEvent.click(
      await screen.findByRole("tab", { name: t("ui.dev.auracast") }),
    );
    mock!.failNextConnect("error.btstack_init");
    fireEvent.click(
      await screen.findByRole("button", { name: t("receiver.start") }),
    );
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(
        t("error.btstack_init"),
      ),
    );
    expect(
      screen.getByRole("button", { name: t("receiver.start") }),
    ).toBeEnabled();
  });
});
