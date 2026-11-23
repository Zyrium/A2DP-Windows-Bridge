// Keyboards and mice through BlueCaster (experimental, issue #413): the
// dialog that turns it on lets the user go on only with both statements,
// the code and the wait; cancelling or a wrong code leaves it off.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../app/App";
import { t } from "../i18n";
import { MOCK_PASSKEY_KEYBOARD, MockCore, type MockScenario } from "../ipc/mock";
import { createCore, type Core } from "../ipc/store";
import { HID_WAIT_SECONDS } from "./HidEnableDialog";
import { hidCandidates, hidCheckHint } from "./HidHost";

let mock: MockCore | null = null;

beforeEach(() => {
  // The countdown only: the mock core's replies keep real timeouts
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
});

afterEach(() => {
  cleanup();
  mock?.dispose();
  mock = null;
  vi.useRealTimers();
});

async function developerScreen(
  scenario: MockScenario = "normal",
): Promise<{ core: Core; calls: string[]; params: Map<string, unknown> }> {
  location.hash = "#/developer";
  const calls: string[] = [];
  // Each method's latest parameters
  const params = new Map<string, unknown>();
  const m = new MockCore({
    speed: 0,
    scenario,
    onCall: (method, p) => {
      calls.push(method);
      params.set(method, p);
    },
  });
  mock = m;
  const core = createCore(m);
  render(() => <App core={core} />);
  await waitFor(() => expect(core.state.snap).not.toBeNull());
  await m.call("settings.set", { settings: { ...core.state.snap!.settings, developer_mode: true } });
  await screen.findByRole("switch", { name: t("ui.dev.hid_toggle") });
  return { core, calls, params };
}

async function openDialog(): Promise<HTMLElement> {
  fireEvent.click(screen.getByRole("switch", { name: t("ui.dev.hid_toggle") }));
  const dialog = await screen.findByRole("dialog");
  await waitFor(() => expect(within(dialog).getByTestId("hid-code").textContent).toMatch(/^\d{6}$/));
  return dialog;
}

const turnOn = (dialog: HTMLElement) =>
  within(dialog).getByRole("button", { name: new RegExp(`^${t("ui.dev.hid_enable_confirm")}`) });

function tickBoth(dialog: HTMLElement) {
  fireEvent.click(within(dialog).getByRole("checkbox", { name: t("ui.dev.hid_confirm_other") }));
  fireEvent.click(within(dialog).getByRole("checkbox", { name: t("ui.dev.hid_confirm_risk") }));
}

function typeCode(dialog: HTMLElement, code: string) {
  fireEvent.input(within(dialog).getByRole("textbox", { name: t("ui.dev.hid_code_input") }), {
    target: { value: code },
  });
}

describe("turning on keyboards and mice", () => {
  it("waits for both statements, the code and the countdown", async () => {
    const { core } = await developerScreen();
    const dialog = await openDialog();
    // The switch stays off until the core turned it on
    expect(screen.getByRole("switch", { name: t("ui.dev.hid_toggle") })).not.toBeChecked();
    expect(turnOn(dialog)).toHaveTextContent(t("ui.dev.hid_enable_wait", HID_WAIT_SECONDS));
    expect(turnOn(dialog)).toBeDisabled();
    tickBoth(dialog);
    typeCode(dialog, within(dialog).getByTestId("hid-code").textContent!);
    // Everything given, the wait not over
    expect(turnOn(dialog)).toBeDisabled();
    vi.advanceTimersByTime(HID_WAIT_SECONDS * 1000 - 1000);
    expect(turnOn(dialog)).toBeDisabled();
    vi.advanceTimersByTime(1000);
    await waitFor(() => expect(turnOn(dialog)).toBeEnabled());
    expect(turnOn(dialog)).toHaveTextContent(t("ui.dev.hid_enable_confirm"));
    fireEvent.click(turnOn(dialog));
    await waitFor(() => expect(core.state.snap!.settings.hid_host).toBe(true));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    // The banner over every screen, without a close button
    const banner = screen.getByText(t("ui.dev.hid_banner_title")).closest(".banner") as HTMLElement;
    expect(within(banner).queryByRole("button", { name: t("modal.close") })).toBeNull();
  });

  it("stays disabled with a box unticked or the code short", async () => {
    await developerScreen();
    const dialog = await openDialog();
    fireEvent.click(within(dialog).getByRole("checkbox", { name: t("ui.dev.hid_confirm_other") }));
    typeCode(dialog, within(dialog).getByTestId("hid-code").textContent!);
    vi.advanceTimersByTime(HID_WAIT_SECONDS * 1000);
    await waitFor(() => expect(turnOn(dialog)).toHaveTextContent(t("ui.dev.hid_enable_confirm")));
    expect(turnOn(dialog)).toBeDisabled();
    fireEvent.click(within(dialog).getByRole("checkbox", { name: t("ui.dev.hid_confirm_risk") }));
    expect(turnOn(dialog)).toBeEnabled();
    typeCode(dialog, "12345");
    expect(turnOn(dialog)).toBeDisabled();
  });

  it("shows a wrong code's error and hands out a new code", async () => {
    const { core, calls } = await developerScreen();
    const dialog = await openDialog();
    const first = within(dialog).getByTestId("hid-code").textContent!;
    tickBoth(dialog);
    typeCode(dialog, first === "000000" ? "111111" : "000000");
    vi.advanceTimersByTime(HID_WAIT_SECONDS * 1000);
    await waitFor(() => expect(turnOn(dialog)).toBeEnabled());
    fireEvent.click(turnOn(dialog));
    expect(await within(dialog).findByText(t("error.hid.wrong_code"))).toBeInTheDocument();
    await waitFor(() => expect(calls.filter((c) => c === "hid.code")).toHaveLength(2));
    expect(core.state.snap!.settings.hid_host).toBe(false);
    // The typed code is cleared: the new one has to be typed
    expect(within(dialog).getByRole("textbox", { name: t("ui.dev.hid_code_input") })).toHaveValue("");
  });

  it("lists the app's risks without the service", async () => {
    await developerScreen();
    const dialog = await openDialog();
    expect(within(dialog).getByText(t("ui.dev.hid_enable_lead"))).toBeInTheDocument();
    const risks = within(dialog)
      .getAllByRole("listitem")
      .map((li) => li.textContent);
    expect(risks).toEqual(
      ["ui.dev.hid_risk_close", "ui.dev.hid_risk_signin", "ui.dev.hid_risk_admin", "ui.dev.hid_risk_games"].map((k) =>
        t(k),
      ),
    );
    expect(screen.getByText(t("ui.dev.hid_toggle_hint"))).toBeInTheDocument();
  });

  it("lists the service's risks in service mode, with the same steps", async () => {
    const { core } = await developerScreen("service");
    expect(core.state.snap!.service_mode).toBe(true);
    const dialog = await openDialog();
    expect(within(dialog).getByText(t("ui.dev.hid_enable_lead_service"))).toBeInTheDocument();
    expect(within(dialog).getByText(t("ui.dev.hid_risk_service"))).toBeInTheDocument();
    expect(within(dialog).getByText(t("ui.dev.hid_risk_sas"))).toBeInTheDocument();
    expect(within(dialog).getByText(t("ui.dev.hid_risk_games"))).toBeInTheDocument();
    expect(within(dialog).queryByText(t("ui.dev.hid_risk_signin"))).toBeNull();
    expect(within(dialog).queryByText(t("ui.dev.hid_risk_admin"))).toBeNull();
    expect(within(dialog).queryByText(t("ui.dev.hid_risk_close"))).toBeNull();
    // Both statements and the code are still asked for
    within(dialog).getByRole("checkbox", { name: t("ui.dev.hid_confirm_other") });
    within(dialog).getByRole("checkbox", { name: t("ui.dev.hid_confirm_risk") });
    within(dialog).getByRole("textbox", { name: t("ui.dev.hid_code_input") });
    expect(screen.getByText(t("ui.dev.hid_toggle_hint_service"))).toBeInTheDocument();
  });

  it("leaves it off when cancelled", async () => {
    const { core, calls } = await developerScreen();
    let dialog = await openDialog();
    tickBoth(dialog);
    fireEvent.click(within(dialog).getByRole("button", { name: t("modal.cancel") }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    dialog = await openDialog();
    // Opened again: nothing ticked
    expect(within(dialog).getByRole("checkbox", { name: t("ui.dev.hid_confirm_other") })).not.toBeChecked();
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls).not.toContain("hid.enable");
    expect(core.state.snap!.settings.hid_host).toBe(false);
    expect(screen.queryByText(t("ui.dev.hid_banner_title"))).toBeNull();
  });

  it("cannot be turned on through the settings", async () => {
    const { core } = await developerScreen();
    await mock!.call("settings.set", { settings: { ...core.state.snap!.settings, hid_host: true } });
    expect(core.state.snap!.settings.hid_host).toBe(false);
  });
});

describe("turning it off", () => {
  it("asks first, from the banner too", async () => {
    const { core } = await developerScreen();
    // Turned on as the dialog would
    const code = ((await mock!.call("hid.code")) as { code: string }).code;
    await mock!.call("hid.enable", { code, other_input_at_hand: true, risk_understood: true });
    const off = await screen.findByRole("button", { name: t("ui.dev.hid_off_confirm") });
    fireEvent.click(off);
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(t("ui.dev.hid_off_text"))).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: t("modal.cancel") }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(core.state.snap!.settings.hid_host).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: t("ui.dev.hid_off_confirm") }));
    const again = await screen.findByRole("dialog");
    fireEvent.click(within(again).getByRole("button", { name: t("ui.dev.hid_off_confirm") }));
    await waitFor(() => expect(core.state.snap!.settings.hid_host).toBe(false));
    await waitFor(() => expect(screen.queryByText(t("ui.dev.hid_banner_title"))).toBeNull());
  });
});

describe("candidates", () => {
  const dev = (address: string, audio: boolean, le_audio = false, saved = false, hid_le: number | null = null) => ({
    address,
    name: "",
    saved,
    audio,
    rssi: null,
    codecs: [],
    sbc: null,
    codec_caps: [],
    codecs_on_request: [],
    le_audio,
    le_audio_set: false,
    hid_le,
    set_addresses: [],
  });

  it("offers scanned classic devices that are not audio and not added", () => {
    const list = [dev("A", false), dev("B", true), dev("C", false, true), dev("D", false, false, true), dev("E", false)];
    const added = [{ address: "e", name: "K", connected: false, le: false }];
    expect(hidCandidates(list, [], added).map((d) => d.address)).toEqual(["A"]);
  });

  it("adds the Bluetooth LE ones, once per device", () => {
    const classic = [dev("A", false), dev("B", false)];
    // B found both ways: listed as the classic one; D already added
    const le = [dev("b", false, false, false, 1), dev("C", false, false, false, 0), dev("D", false, false, false, 1)];
    const added = [{ address: "D", name: "M", connected: true, le: true }];
    const got = hidCandidates(classic, le, added);
    expect(got.map((d) => [d.address, d.hid_le])).toEqual([
      ["A", null],
      ["B", null],
      ["C", 0],
    ]);
  });

  it("adds an LE one with its address type", async () => {
    const { core, params } = await developerScreen();
    const code = ((await mock!.call("hid.code")) as { code: string }).code;
    await mock!.call("hid.enable", { code, other_input_at_hand: true, risk_understood: true });
    fireEvent.click(await screen.findByRole("button", { name: t("connection.scan") }));
    // The mock's scan: MX Keys both ways (classic listed), a mouse over LE only
    const mouse = await screen.findByText("LE Mouse");
    expect(screen.getAllByText("MX Keys")).toHaveLength(1);
    // Never on the audio device list
    expect(core.state.snap!.devices.some((d) => d.address === "E1:5A:2B:3C:4D:5E")).toBe(false);
    const row = mouse.closest("li") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: t("ui.devices.add") }));
    await waitFor(() => expect(params.get("hid.add")).toEqual({ address: "E1:5A:2B:3C:4D:5E", name: "LE Mouse", le: 1 }));
    await waitFor(() => expect(core.state.snap!.hid_devices.find((d) => d.le)?.name).toBe("LE Mouse"));
  });
});

describe("pairing with a number", () => {
  async function hidOnScreen() {
    const r = await developerScreen();
    const code = ((await mock!.call("hid.code")) as { code: string }).code;
    await mock!.call("hid.enable", { code, other_input_at_hand: true, risk_understood: true });
    await screen.findByRole("button", { name: t("connection.scan") });
    return r;
  }

  it("shows the number and the instruction while the keyboard pairs", async () => {
    await hidOnScreen();
    expect(screen.queryByTestId("hid-passkey")).toBeNull();
    mock!.setHidPasskey({ address: MOCK_PASSKEY_KEYBOARD, passkey: "042137" });
    expect(await screen.findByTestId("hid-passkey")).toHaveTextContent("042137");
    const block = screen.getByTestId("hid-passkey").parentElement!;
    expect(within(block).getByText(t("ui.dev.hid_passkey_hint"))).toBeInTheDocument();
    mock!.setHidPasskey(null);
    await waitFor(() => expect(screen.queryByTestId("hid-passkey")).toBeNull());
  });

  it("names the device being added", async () => {
    await hidOnScreen();
    void mock!.call("hid.add", { address: "AA:BB:CC:DD:EE:01", name: "Desk keyboard", le: null });
    await screen.findByText("Desk keyboard");
    mock!.setHidPasskey({ address: "aa:bb:cc:dd:ee:01", passkey: "000001" });
    const block = (await screen.findByTestId("hid-passkey")).parentElement!;
    expect(within(block).getByText("Desk keyboard")).toBeInTheDocument();
  });

  it("the mock's keyboard shows one, then clears it once paired", async () => {
    const { core } = await hidOnScreen();
    const shown = vi.spyOn(mock!, "setHidPasskey");
    await mock!.call("hid.add", { address: MOCK_PASSKEY_KEYBOARD, name: "MX Keys", le: null });
    expect(shown.mock.calls.map((c) => c[0]?.passkey ?? null)).toEqual([expect.stringMatching(/^\d{6}$/), null]);
    expect(core.state.snap!.hid_passkey).toBeNull();
    expect(core.state.snap!.hid_devices.find((d) => d.address === MOCK_PASSKEY_KEYBOARD)?.connected).toBe(true);
  });
});

describe("the check of a device being added", () => {
  const MOUSE = "AA:BB:CC:DD:EE:02";

  async function adding(outcome: "manual" | "input" = "manual") {
    const r = await developerScreen();
    const code = ((await mock!.call("hid.code")) as { code: string }).code;
    await mock!.call("hid.enable", { code, other_input_at_hand: true, risk_understood: true });
    await screen.findByRole("button", { name: t("connection.scan") });
    mock!.hidCheckOutcome = outcome;
    await mock!.call("hid.add", { address: MOUSE, name: "Desk mouse", le: 1 });
    return { ...r, block: await screen.findByTestId("hid-check") };
  }

  it("asks for input with a countdown and keeps the device once its input arrives", async () => {
    const { core, block } = await adding();
    expect(within(block).getByText("Desk mouse")).toBeInTheDocument();
    expect(within(block).getByText(t("ui.dev.hid_check_mouse"))).toBeInTheDocument();
    expect(within(block).getByTestId("hid-check-left")).toHaveTextContent(t("ui.dev.hid_check_left", 30));
    vi.advanceTimersByTime(5000);
    await waitFor(() =>
      expect(within(block).getByTestId("hid-check-left")).toHaveTextContent(t("ui.dev.hid_check_left", 25)),
    );
    mock!.hidInput(MOUSE);
    await waitFor(() => expect(screen.queryByTestId("hid-check")).toBeNull());
    expect(core.state.snap!.hid_devices.some((d) => d.address === MOUSE)).toBe(true);
    expect(screen.queryByTestId("hid-rolled-back")).toBeNull();
  });

  it("says so when the countdown removed the device, and Try again adds it again", async () => {
    const { core, params } = await adding();
    mock!.hidCheckTimeout(MOUSE);
    const notice = await screen.findByTestId("hid-rolled-back");
    expect(notice).toHaveTextContent(t("ui.dev.hid_rolled_back", "Desk mouse"));
    expect(screen.queryByTestId("hid-check")).toBeNull();
    expect(core.state.snap!.hid_devices.some((d) => d.address === MOUSE)).toBe(false);
    params.delete("hid.add");
    mock!.hidCheckOutcome = "manual";
    fireEvent.click(within(notice).getByRole("button", { name: t("ui.fix.retry") }));
    await waitFor(() => expect(params.get("hid.add")).toEqual({ address: MOUSE, name: "Desk mouse", le: 1 }));
    await screen.findByTestId("hid-check");
    await waitFor(() => expect(screen.queryByTestId("hid-rolled-back")).toBeNull());
  });

  it("Close dismisses the notice in the core", async () => {
    const { calls } = await adding();
    mock!.hidCheckTimeout(MOUSE);
    const notice = await screen.findByTestId("hid-rolled-back");
    fireEvent.click(within(notice).getByRole("button", { name: t("modal.close") }));
    await waitFor(() => expect(screen.queryByTestId("hid-rolled-back")).toBeNull());
    expect(calls).toContain("hid.check_dismiss");
  });

  it("Remove ends the countdown at once, without a notice", async () => {
    const { core, calls, block } = await adding();
    fireEvent.click(within(block).getByRole("button", { name: t("ui.device.forget") }));
    await waitFor(() => expect(screen.queryByTestId("hid-check")).toBeNull());
    expect(calls).toContain("hid.check_cancel");
    expect(core.state.snap!.hid_devices.some((d) => d.address === MOUSE)).toBe(false);
    expect(screen.queryByTestId("hid-rolled-back")).toBeNull();
  });

  it("a keyboard is asked for a key, an unknown device for a key or button", () => {
    expect(hidCheckHint("keyboard")).toBe(t("ui.dev.hid_check_keyboard"));
    expect(hidCheckHint("both")).toBe(t("ui.dev.hid_check_any"));
    expect(hidCheckHint("")).toBe(t("ui.dev.hid_check_any"));
  });
});

describe("the self-test of the input path", () => {
  it("a failure at turning on turns the feature off again and says why", async () => {
    await developerScreen();
    mock!.hidSelfTestFails = "refused";
    const dialog = await openDialog();
    tickBoth(dialog);
    typeCode(dialog, within(dialog).getByTestId("hid-code").textContent!);
    vi.advanceTimersByTime(HID_WAIT_SECONDS * 1000);
    await waitFor(() => expect(turnOn(dialog)).toBeEnabled());
    fireEvent.click(turnOn(dialog));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(t("ui.dev.hid_test_off_refused"));
    expect(screen.getByRole("switch", { name: t("ui.dev.hid_toggle") })).not.toBeChecked();
    expect(screen.queryByText(t("ui.dev.hid_banner_title"))).toBeNull();
  });

  it("a failure at a later start only warns in the banner", async () => {
    const { core } = await developerScreen();
    const code = ((await mock!.call("hid.code")) as { code: string }).code;
    await mock!.call("hid.enable", { code, other_input_at_hand: true, risk_understood: true });
    await screen.findByText(t("ui.dev.hid_banner_title"));
    expect(screen.queryByTestId("hid-test-failed")).toBeNull();
    mock!.setHidHealth({ state: "failed", reason: "helper" });
    expect(await screen.findByTestId("hid-test-failed")).toHaveTextContent(t("ui.dev.hid_banner_test_failed"));
    // Still on
    expect(core.state.snap!.settings.hid_host).toBe(true);
    mock!.setHidHealth({ state: "ok", reason: null });
    await waitFor(() => expect(screen.queryByTestId("hid-test-failed")).toBeNull());
  });
});
