// The two windows' roots. The main window: navigation on the left, the
// screen on the right, the first-run setup over both while it runs. The
// flyout: one compact view.

import { createEffect, createSignal, For, Match, on, onCleanup, Show, Switch } from "solid-js";
import { CablesOnce } from "../components/CableNotes";
import { Icon } from "../components/Icon";
import { NoticeLine } from "../components/Notice";
import type { IconName } from "../components/icons";
import { BatteryLevel, CodecBadge, codecDetails, codecName, codecStopText, showsBattery, stateLabel } from "../components/stream";
import { lang, setLanguage, t } from "../i18n";
import type { Core } from "../ipc/store";
import { About } from "../screens/About";
import { Adapter } from "../screens/Adapter";
import { Codecs } from "../screens/Codecs";
import { Devices } from "../screens/Devices";
import { Developer } from "../screens/Developer";
import { Diagnostics } from "../screens/Diagnostics";
import { Fones } from "../screens/fones/Fones";
import { ft } from "../screens/fones/strings";
import { Flyout } from "../screens/Flyout";
import { entryState, Home, MicRow } from "../screens/Home";
import { audioEntries, entryByKey, type AudioEntry } from "../lib/profile";
import { Settings } from "../screens/Settings";
import { Mixer } from "../screens/Mixer";
import { Setup, needsSetup } from "../screens/Setup";
import { applyTheme, followAccent } from "../theme";
import { AppProvider, useApp, type RouteName } from "./context";
import { mixerShown, saveSettings } from "./settings";
import { HidNotice } from "../components/HidHost";
import { ServiceConnectionNotice, ServiceNewerNotice } from "../components/ServiceAddon";
import { CaptureSign, CrashNotice, UpdateNotice } from "./indicators";
import "./shell.css";

export function App(props: { core: Core }) {
  const core = props.core;
  // Language and theme come from the core's settings
  createEffect(() => {
    const l = core.state.snap?.settings.language;
    if (l) void setLanguage(l);
  });
  createEffect(() => {
    const theme = core.state.snap?.settings.theme ?? "system";
    onCleanup(applyTheme(theme, core.transport));
  });
  followAccent(core.transport);
  if (core.transport.mica) document.documentElement.classList.add("mica");
  document.documentElement.classList.add(`win-${core.transport.window}`);

  return (
    <AppProvider core={core}>
      <Show when={core.transport.window === "flyout"} fallback={<MainWindow />}>
        <Gate compact>
          <Flyout />
        </Gate>
      </Show>
    </AppProvider>
  );
}

/** The screen until the core answered: connecting, missing, wrong version */
function Gate(props: { compact?: boolean; children: any }) {
  const { core } = useApp();
  const ready = () => core.state.conn.state === "connected" && core.state.snap !== null;
  return (
    <Show when={ready()} fallback={<ConnectionScreen compact={props.compact} />}>
      {props.children}
    </Show>
  );
}

function ConnectionScreen(props: { compact?: boolean }) {
  const { core } = useApp();
  const conn = () => core.state.conn;
  return (
    <div class="conn-screen" classList={{ compact: props.compact }} role="status" aria-live="polite">
      <img src="/app-icon.png" alt="" class="conn-logo" width="64" height="64" />
      <Switch>
        <Match when={conn().state === "core_missing"}>
          <h1>{t("ui.conn.missing_title")}</h1>
          <p>{t("ui.conn.missing_body")}</p>
          <p class="muted mono">{(conn() as { detail: string }).detail}</p>
        </Match>
        <Match when={conn().state === "rejected"}>
          <h1>{t("ui.conn.rejected_title")}</h1>
          <p>{t("ui.conn.rejected_body")}</p>
          <p class="muted mono">{(conn() as { reason: string }).reason}</p>
        </Match>
        <Match when={conn().state === "service_needs_update"}>
          <ServiceConnectionNotice
            stopped={false}
            version={(conn() as { version: string }).version}
            app={(conn() as { app: string }).app}
          />
        </Match>
        <Match when={conn().state === "service_newer"}>
          <ServiceNewerNotice
            version={(conn() as { version: string }).version}
            app={(conn() as { app: string }).app}
          />
        </Match>
        <Match when={conn().state === "service_stopped"}>
          <ServiceConnectionNotice stopped />
        </Match>
        <Match when={true}>
          <span class="spinner big" aria-hidden="true" />
          <h1>{t("ui.conn.connecting")}</h1>
        </Match>
      </Switch>
    </div>
  );
}

const NAV: { name: RouteName; icon: IconName | "earbuds"; label: () => string }[] = [
  { name: "home", icon: "home", label: () => t("ui.nav.home") },
  { name: "codecs", icon: "headphones", label: () => t("ui.nav.codecs") },
  // Its texts are the screen's own for now (screens/fones/strings.ts)
  { name: "fones", icon: "earbuds", label: () => ft("nav") },
  { name: "devices", icon: "bluetooth", label: () => t("ui.nav.devices") },
  { name: "adapter", icon: "usb_stick", label: () => t("ui.nav.adapter") },
  { name: "mixer", icon: "speaker_2", label: () => t("ui.nav.mixer") },
  { name: "diagnostics", icon: "pulse", label: () => t("ui.nav.diagnostics") },
  { name: "settings", icon: "settings", label: () => t("ui.nav.settings") },
  { name: "about", icon: "info", label: () => t("ui.nav.about") },
  // Shown in developer mode only: last, out of the way
  { name: "developer", icon: "wrench", label: () => t("ui.nav.developer") },
];

/** The Fluent icons, and the earbuds of the Fones item (none in the set) */
function NavIcon(props: { name: IconName | "earbuds" }) {
  return (
    <Show when={props.name === "earbuds"} fallback={<Icon name={props.name as IconName} />}>
      <svg class="icon" width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.1" aria-hidden="true">
        <path d="M6 3.5a3.5 3.5 0 0 1 3.5 3.5v1.5a2.5 2.5 0 0 1-1.5 2.3V16a1 1 0 0 1-2 0v-5.2A3.5 3.5 0 0 1 6 3.5z" />
        <path d="M14 3.5a3.5 3.5 0 0 0-3.5 3.5v1.5a2.5 2.5 0 0 0 1.5 2.3V16a1 1 0 0 0 2 0v-5.2A3.5 3.5 0 0 0 14 3.5z" />
      </svg>
    </Show>
  );
}

function MainWindow() {
  const app = useApp();
  const { core, route } = app;
  const shown = (n: { name: RouteName }) => {
    if (n.name === "mixer") return mixerShown(core.state.snap?.settings);
    return n.name !== "developer" || core.state.snap?.settings.developer_mode;
  };
  const [collapsed, setCollapsed] = createSignal<boolean | null>(null);

  // The first-run setup: decided once when the state first arrives (and
  // when asked for with #/setup); it stays until finished or skipped, even
  // when creating the profile makes it no longer "needed". Only until it
  // was finished or skipped once (issue #86): with every device removed
  // later, Home adds one. A folder set up without it needs it no more.
  createEffect(
    on(
      () => core.state.snap !== null,
      (has) => {
        const snap = core.state.snap;
        if (!has || !snap || snap.settings.setup_done) return;
        if (needsSetup(snap)) app.setSetupActive(true);
        else void saveSettings(app, { setup_done: true });
      },
    ),
  );
  createEffect(() => {
    if (route().name === "setup") app.setSetupActive(true);
  });

  // The log lines feed the connection flow, so the main window listens
  // while it is open (the core only sends them to subscribers)
  const releaseLogs = core.wantLogs("info");
  onCleanup(releaseLogs);

  // Ctrl+Shift+D opens the diagnostics (v1's Debug Console shortcut)
  const onKey = (e: KeyboardEvent) => {
    if (isDiagnosticsShortcut(e) && !app.setupActive()) {
      e.preventDefault();
      app.navigate({ name: "diagnostics" });
    }
  };
  window.addEventListener("keydown", onKey);
  onCleanup(() => window.removeEventListener("keydown", onKey));

  // "Minimize to System Tray": the tray icon is the core's, so minimizing
  // only has to take the window off the taskbar. The core says once that
  // BlueCaster still runs in the tray (closing the window does the same, in
  // src-tauri/src/windows.rs)
  core.transport.onMinimized(() => {
    if (core.state.snap?.settings.minimize_to_tray) void hideToTray(core);
  });

  return (
    <Gate>
      <Show
        when={!app.setupActive()}
        fallback={
          <main class="setup-wrap" id="main">
            <Setup />
          </main>
        }
      >
        <div class="shell" classList={{ "nav-collapsed": collapsed() === true, "nav-expanded": collapsed() === false }}>
          <a class="skip-link" href="#main">
            {t("ui.skip_to_content")}
          </a>
          <nav class="nav" aria-label={t("ui.nav.label")}>
            <div class="nav-top">
              <button
                type="button"
                class="btn btn-icon nav-toggle"
                aria-label={t("ui.nav.toggle")}
                title={t("ui.nav.toggle")}
                aria-expanded={collapsed() !== true}
                onClick={() => setCollapsed(!(collapsed() ?? window.innerWidth < 860))}
              >
                <Icon name="navigation" />
              </button>
            </div>
            <ul class="nav-list">
              <For each={NAV.filter(shown)}>
                {(item) => (
                  <li>
                    <a
                      href={`#/${item.name}`}
                      class="nav-item"
                      classList={{ on: route().name === item.name }}
                      aria-current={route().name === item.name ? "page" : undefined}
                      title={item.label()}
                    >
                      <NavIcon name={item.icon} />
                      <span class="nav-label">{item.label()}</span>
                    </a>
                  </li>
                )}
              </For>
            </ul>
            <CaptureSign />
            <StatusPill />
          </nav>
          <main class="content" id="main" tabindex="-1" lang={lang()}>
            <div class="app-notices">
              <HidNotice />
              <CrashNotice />
              <UpdateNotice />
            </div>
            <Switch>
              <Match when={route().name === "home"}>
                <Home />
              </Match>
              <Match when={route().name === "codecs"}>
                <Codecs />
              </Match>
              <Match when={route().name === "fones"}>
                <Fones tab={route().arg} />
              </Match>
              <Match when={route().name === "devices"}>
                <Devices />
              </Match>
              <Match when={route().name === "diagnostics"}>
                <Diagnostics />
              </Match>
              <Match when={route().name === "mixer" && mixerShown(core.state.snap?.settings)}>
                <Mixer />
              </Match>
              <Match when={route().name === "developer" && core.state.snap?.settings.developer_mode}>
                <Developer />
              </Match>
              <Match when={route().name === "adapter"}>
                <Adapter />
              </Match>
              <Match when={route().name === "about"}>
                <About />
              </Match>
              <Match when={route().name === "settings" || route().name === "setup"}>
                <Settings />
              </Match>
            </Switch>
          </main>
          <CablesOnce />
        </div>
      </Show>
      <NoticeLine />
    </Gate>
  );
}

/** Ctrl+Shift+D, whatever the keyboard layout writes for the key */
export function isDiagnosticsShortcut(e: Pick<KeyboardEvent, "ctrlKey" | "shiftKey" | "altKey" | "metaKey" | "code" | "key">): boolean {
  return e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey && (e.code === "KeyD" || e.key.toLowerCase() === "d");
}

/** Hides the main window to the tray and lets the core show its one-time
 *  "still running in the tray" balloon */
export async function hideToTray(core: Core): Promise<void> {
  await core.transport.hideWindow();
  await core.request("app.hidden_to_tray").catch(() => {});
}

/** Always visible (design principle 2): each device and its state, and the
 *  codec and what it runs with for the one streaming, so other screens need
 *  no trip to Home (issue #86) */
function StatusPill() {
  const { core } = useApp();
  const snap = () => core.state.snap!;
  const entries = () => audioEntries(snap());
  return (
    <div class="status-pills">
      <Show when={entries().length > 0} fallback={<DevicePill entry={null} />}>
        <For each={entries().map((e) => e.key)}>
          {(key) => {
            const entry = entryByKey(entries, key);
            return <DevicePill entry={entry()} />;
          }}
        </For>
      </Show>
    </div>
  );
}

/** One device's block (issue #86): the device and its state; battery and
 *  codec for the one streaming, and its microphone's button and line as on
 *  Home's card. Without devices: the state alone. The whole block opens
 *  Home (its dot is the button, stretched over it), the microphone's button
 *  apart. */
function DevicePill(props: { entry: AudioEntry | null }) {
  const { core, navigate } = useApp();
  const snap = () => core.state.snap!;
  const stream = () => snap().stream;
  const state = () => (props.entry ? entryState(snap(), props.entry) : stream().state);
  const live = () => state() !== "idle";
  const device = () => (props.entry ? props.entry.name || props.entry.address : "");
  const details = () => {
    const i = stream().info;
    return i ? codecDetails(i).join(" ") : "";
  };
  const info = () => (live() ? stream().info : null);
  // An error the codec caused, named (issue #86)
  const stateText = () => codecStopText({ ...stream(), state: state() }) ?? stateLabel(state());
  // Also the name while collapsed to the dot
  const label = () => {
    const i = info();
    return [device(), stateText(), i ? `${codecName(i)} ${details()}` : ""].filter((x) => x).join(", ");
  };
  return (
    <div class={`status-pill st-${state()}`} title={label()}>
      <button
        type="button"
        class="status-pill-open dot"
        onClick={() => navigate({ name: "home" })}
        aria-label={label()}
      />
      <span class="status-pill-text">
        {/* As Home's card: the device, battery and state, the codec */}
        <Show when={device()}>
          <span class="status-pill-device clamp-2">{device()}</span>
        </Show>
        <span class="status-pill-line">
          <Show when={live() && showsBattery(stream())}>
            <BatteryLevel class="status-pill-sub" percent={stream().battery} />
            <span class="status-pill-sub" aria-hidden="true">
              |
            </span>
          </Show>
          <span class="status-pill-state">{stateText()}</span>
        </span>
        {/* The codec, then its details below it, as on Home and in the tray (issue #86) */}
        <Show when={info()}>
          <CodecBadge />
          <Show when={props.entry}>{(e) => <MicRow entry={e()} />}</Show>
        </Show>
      </span>
    </div>
  );
}
