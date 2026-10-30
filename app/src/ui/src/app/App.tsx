// The two windows' roots. The main window: navigation on the left, the
// screen on the right, the first-run setup over both while it runs. The
// flyout: one compact view.

import { createEffect, createSignal, For, Match, on, onCleanup, Show, Switch } from "solid-js";
import { Icon } from "../components/Icon";
import { NoticeLine } from "../components/Notice";
import type { IconName } from "../components/icons";
import { CodecBadge, stateLabel } from "../components/stream";
import { lang, setLanguage, t } from "../i18n";
import type { Core } from "../ipc/store";
import { Adapter } from "../screens/Adapter";
import { Diagnostics } from "../screens/Diagnostics";
import { Fones } from "../screens/fones/Fones";
import { ft } from "../screens/fones/strings";
import { Flyout } from "../screens/Flyout";
import { Home } from "../screens/Home";
import { Profiles } from "../screens/Profiles";
import { Settings } from "../screens/Settings";
import { Setup, needsSetup } from "../screens/Setup";
import { applyTheme } from "../theme";
import { AppProvider, useApp, type RouteName } from "./context";
import { CaptureSign, UpdateNotice } from "./indicators";
import "./shell.css";

export function App(props: { core: Core }) {
  const core = props.core;
  // Language and theme come from the core's settings
  createEffect(() => {
    const l = core.state.snap?.settings.language;
    if (l) void setLanguage(l);
  });
  createEffect(() => {
    const theme = core.state.snap?.settings.theme ?? "dark";
    onCleanup(applyTheme(theme, core.transport));
  });
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
  { name: "profiles", icon: "headphones", label: () => t("ui.nav.profiles") },
  // Its texts are the screen's own for now (screens/fones/strings.ts)
  { name: "fones", icon: "earbuds", label: () => ft("nav") },
  { name: "diagnostics", icon: "pulse", label: () => t("ui.nav.diagnostics") },
  { name: "adapter", icon: "usb_stick", label: () => t("ui.nav.adapter") },
  { name: "settings", icon: "settings", label: () => t("ui.nav.settings") },
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
  const [collapsed, setCollapsed] = createSignal<boolean | null>(null);

  // The first-run setup: decided once when the state first arrives (and
  // when asked for with #/setup); it stays until finished or skipped, even
  // when creating the profile makes it no longer "needed"
  createEffect(
    on(
      () => core.state.snap !== null,
      (has) => {
        if (has && needsSetup(core.state.snap!)) app.setSetupActive(true);
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
  // A2DPWB still runs in the tray (closing the window does the same, in
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
              <img src="/app-icon.png" alt="" class="nav-logo" width="20" height="20" />
              <span class="nav-title">{t("app.title")}</span>
            </div>
            <ul class="nav-list">
              <For each={NAV}>
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
              <UpdateNotice />
            </div>
            <Switch>
              <Match when={route().name === "home"}>
                <Home />
              </Match>
              <Match when={route().name === "profiles"}>
                <Profiles />
              </Match>
              <Match when={route().name === "fones"}>
                <Fones tab={route().arg} />
              </Match>
              <Match when={route().name === "diagnostics"}>
                <Diagnostics />
              </Match>
              <Match when={route().name === "adapter"}>
                <Adapter />
              </Match>
              <Match when={route().name === "settings" || route().name === "setup"}>
                <Settings />
              </Match>
            </Switch>
          </main>
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

/** Always visible (design principle 2): stream state, codec, bitrate */
function StatusPill() {
  const { core, navigate } = useApp();
  const stream = () => core.state.snap!.stream;
  return (
    <button
      type="button"
      class={`status-pill st-${stream().state}`}
      onClick={() => navigate({ name: "home" })}
      aria-label={`${stateLabel(stream().state)}`}
      title={stateLabel(stream().state)}
    >
      <span class="dot" aria-hidden="true" />
      <span class="status-pill-text">
        <span class="status-pill-state">{stateLabel(stream().state)}</span>
        <Show when={stream().info}>
          <CodecBadge compact />
        </Show>
      </span>
    </button>
  );
}
