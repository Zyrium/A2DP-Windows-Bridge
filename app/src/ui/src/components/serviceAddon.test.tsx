// The service add-on (issue #413): the Developer screen's row (add, remove,
// the data kept unless chosen), and the connection screen when the service
// is another version or stopped, which needs no developer mode.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../app/App";
import { t } from "../i18n";
import { MOCK_NEW_SERVICE, MOCK_OLD_SERVICE, MockCore, type MockScenario } from "../ipc/mock";
import { createCore } from "../ipc/store";

let mock: MockCore | null = null;

afterEach(() => {
  cleanup();
  mock?.dispose();
  mock = null;
});

function start(hash: string, scenario: MockScenario = "normal", setup?: (m: MockCore) => void) {
  location.hash = hash;
  const setups: { action: string; remove_data: boolean }[] = [];
  const m = new MockCore({
    speed: 0,
    scenario,
    onCall: (method, p) => {
      if (method === "service.setup") setups.push(p as { action: string; remove_data: boolean });
    },
  });
  setup?.(m);
  mock = m;
  const core = createCore(m);
  render(() => <App core={core} />);
  return { core, setups, mock: m };
}

async function developerRow(scenario: MockScenario = "normal", setup?: (m: MockCore) => void) {
  const r = start("#/developer", scenario, (m) => {
    m.setDeveloperMode();
    setup?.(m);
  });
  const row = await screen.findByTestId("service-row");
  return { ...r, row };
}

describe("the service add-on on the Developer screen", () => {
  it("is offered by what it gives, and adding it asks first", async () => {
    const { core, setups, row } = await developerRow();
    expect(within(row).getByText(t("ui.service.row"))).toBeInTheDocument();
    expect(within(row).getByText(t("ui.service.state_not_added"))).toBeInTheDocument();
    expect(within(row).queryByRole("button", { name: t("ui.service.remove") })).toBeNull();
    fireEvent.click(within(row).getByRole("button", { name: t("ui.service.add") }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(t("ui.service.add_all_users"))).toBeInTheDocument();
    expect(within(dialog).getByText(t("ui.service.add_headphones"))).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: t("ui.service.add") }));
    await waitFor(() => expect(setups).toEqual([{ action: "install", remove_data: false }]));
    // The window connects to the service: the row is drawn again
    await waitFor(() => expect(core.state.snap?.service_mode).toBe(true));
    const after = await screen.findByTestId("service-row");
    await within(after).findByText(t("ui.service.state_running"));
    // Adding the service does not turn keyboards and mice on
    expect(core.state.snap!.settings.hid_host).toBe(false);
    expect(screen.getByRole("switch", { name: t("ui.dev.hid_toggle") })).not.toBeChecked();
  });

  it("says so when the administrator prompt is declined", async () => {
    const { setups, row } = await developerRow("normal", (m) => (m.serviceDeclines = true));
    fireEvent.click(within(row).getByRole("button", { name: t("ui.service.add") }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: t("ui.service.add") }));
    expect(await within(row).findByRole("alert")).toHaveTextContent(t("error.admin_declined"));
    expect(setups).toHaveLength(1);
    expect(within(row).getByText(t("ui.service.state_not_added"))).toBeInTheDocument();
  });

  it("shows the set-up tool's error code", async () => {
    const { row } = await developerRow("normal", (m) => (m.serviceExitCode = 7));
    fireEvent.click(within(row).getByRole("button", { name: t("ui.service.add") }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: t("ui.service.add") }));
    expect(await within(row).findByRole("alert")).toHaveTextContent(t("ui.service.failed", 7));
  });

  it("removes it after asking, keeping its data unless chosen", async () => {
    const { core, setups, row } = await developerRow("service");
    expect(within(row).getByText(t("ui.service.state_running"))).toBeInTheDocument();
    expect(within(row).queryByRole("button", { name: t("ui.service.add") })).toBeNull();
    fireEvent.click(within(row).getByRole("button", { name: t("ui.service.remove") }));
    let dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(t("ui.service.remove_text"))).toBeInTheDocument();
    const data = within(dialog).getByRole("switch", { name: t("ui.service.remove_data") });
    expect(data).not.toBeChecked();
    // Cancelled with the data ticked: opened again, it is off
    fireEvent.click(data);
    fireEvent.click(within(dialog).getByRole("button", { name: t("modal.cancel") }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    fireEvent.click(within(row).getByRole("button", { name: t("ui.service.remove") }));
    dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByRole("switch", { name: t("ui.service.remove_data") })).not.toBeChecked();
    fireEvent.click(within(dialog).getByRole("button", { name: t("ui.service.remove") }));
    await waitFor(() => expect(setups).toEqual([{ action: "remove", remove_data: false }]));
    // The user's own core serves the window again
    await waitFor(() => expect(core.state.snap?.service_mode).toBe(false));
    await within(await screen.findByTestId("service-row")).findByText(t("ui.service.state_not_added"));
  });

  it("deletes the data only when ticked", async () => {
    const { setups, row } = await developerRow("service");
    fireEvent.click(within(row).getByRole("button", { name: t("ui.service.remove") }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("switch", { name: t("ui.service.remove_data") }));
    fireEvent.click(within(dialog).getByRole("button", { name: t("ui.service.remove") }));
    await waitFor(() => expect(setups).toEqual([{ action: "remove", remove_data: true }]));
  });

  it("is not on the Developer screen's way without developer mode", async () => {
    start("#/settings");
    await screen.findByRole("heading", { level: 1, name: t("ui.nav.settings") });
    expect(screen.queryByTestId("service-row")).toBeNull();
  });
});

describe("Settings while connected to the service", () => {
  it("shows Start with Windows on and fixed: the service starts with Windows", async () => {
    start("#/settings", "service");
    const s = await screen.findByRole("switch", { name: t("settings.start_with_windows") });
    expect(s).toBeChecked();
    expect(s).toBeDisabled();
  });

  it("leaves it to the user without the service", async () => {
    start("#/settings");
    const s = await screen.findByRole("switch", { name: t("settings.start_with_windows") });
    expect(s).toBeEnabled();
  });
});

describe("when the service needs an update", () => {
  it("tells the user on the connection screen and updates it", async () => {
    const { core, setups } = start("#/home", "service_needs_update");
    await screen.findByRole("heading", { name: t("ui.service.outdated_title") });
    expect(screen.getByText(t("ui.service.outdated_body"))).toBeInTheDocument();
    expect(screen.getByText(t("ui.service.versions", mock!.version, MOCK_OLD_SERVICE))).toBeInTheDocument();
    // No developer mode needed, and no core of the user's own serves
    expect(core.state.snap).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: t("ui.service.update") }));
    await waitFor(() => expect(setups).toEqual([{ action: "update", remove_data: false }]));
    await waitFor(() => expect(core.state.snap?.service_mode).toBe(true));
    expect(screen.queryByRole("heading", { name: t("ui.service.outdated_title") })).toBeNull();
  });

  it("stays when the prompt is declined", async () => {
    const { core } = start("#/home", "service_needs_update", (m) => (m.serviceDeclines = true));
    fireEvent.click(await screen.findByRole("button", { name: t("ui.service.update") }));
    expect(await screen.findByRole("alert")).toHaveTextContent(t("error.admin_declined"));
    expect(screen.getByRole("heading", { name: t("ui.service.outdated_title") })).toBeInTheDocument();
    expect(core.state.snap).toBeNull();
  });

  it("can remove the service instead", async () => {
    const { core, setups } = start("#/home", "service_needs_update");
    fireEvent.click(await screen.findByRole("button", { name: t("ui.service.remove") }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: t("ui.service.remove") }));
    await waitFor(() => expect(setups).toEqual([{ action: "remove", remove_data: false }]));
    await waitFor(() => expect(core.state.snap?.service_mode).toBe(false));
  });
});

describe("when the service is newer than the app", () => {
  it("asks for the app's update and never sets the service up from here", async () => {
    const { core, setups } = start("#/home", "service_newer");
    await screen.findByRole("heading", { name: t("ui.service.newer_title") });
    expect(screen.getByText(t("ui.service.versions", mock!.version, MOCK_NEW_SERVICE))).toBeInTheDocument();
    expect(screen.getByRole("button", { name: t("ui.settings.open_download") })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: t("ui.service.update") })).toBeNull();
    expect(screen.queryByRole("button", { name: t("ui.service.remove") })).toBeNull();
    expect(setups).toEqual([]);
    expect(core.state.snap).toBeNull();
  });
});

describe("when the service is stopped", () => {
  it("offers to start it (an update, which an administrator approves)", async () => {
    const { core, setups } = start("#/home", "service_stopped");
    await screen.findByRole("heading", { name: t("ui.service.stopped_title") });
    expect(screen.getByText(t("ui.service.stopped_body"))).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: t("ui.service.start") }));
    await waitFor(() => expect(setups).toEqual([{ action: "update", remove_data: false }]));
    await waitFor(() => expect(core.state.snap?.service_mode).toBe(true));
  });
});
