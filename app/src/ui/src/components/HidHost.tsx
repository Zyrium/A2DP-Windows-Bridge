// Keyboards and mice through BlueCaster (experimental, developer mode,
// issue #413): the Developer section with its switch and devices, the
// question before turning it off, and the banner over every screen while
// it is on. Turning it on goes through HidEnableDialog.
//
// A device being added stays only once its input reaches Windows: the
// core counts down (Snapshot.hid_checks) and removes it when no input
// arrives in time, also with this window closed. The UI only shows the
// countdown and offers Remove; nothing here can confirm a device.

import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { useApp } from "../app/context";
import { t } from "../i18n";
import type { Device, HidCheck, HidCounts, HidDevice, HidPasskey, HidRolledBack, Settings } from "../ipc/types";
import { Banner, Button, Dialog, Section, Toggle } from "./controls";
import { SCAN_SECONDS } from "./DevicePicker";
import { HidEnableDialog } from "./HidEnableDialog";
import { Icon } from "./Icon";
import { ServiceRow } from "./ServiceAddon";

/** How often the counts are read while the section is on screen */
const COUNTS_EVERY_MS = 2000;

/** In force: developer mode on and the feature turned on */
export function hidOn(s: Pick<Settings, "developer_mode" | "hid_host"> | undefined): boolean {
  return !!s?.developer_mode && !!s.hid_host;
}

/** The scanned devices that may be keyboards or mice, not added yet:
 *  classic ones that are not audio devices, then the Bluetooth LE keyboards
 *  and mice. A device found both ways is listed once, as the classic one. */
export function hidCandidates(
  devices: readonly Device[],
  le: readonly Device[],
  added: readonly HidDevice[],
): Device[] {
  const taken = new Set(added.map((d) => d.address.toUpperCase()));
  const classic = devices.filter(
    (d) => !d.audio && !d.le_audio && d.hid_le == null && !d.saved && !taken.has(d.address.toUpperCase()),
  );
  const listed = new Set(classic.map((d) => d.address.toUpperCase()));
  const leOnly = le.filter((d) => d.hid_le != null && !taken.has(d.address.toUpperCase()) && !listed.has(d.address.toUpperCase()));
  return [...classic, ...leOnly];
}

/** Asks before turning it off: the mouse in hand may be one of them */
export function HidOffDialog(props: { open: boolean; onClose: () => void }) {
  const app = useApp();
  const [busy, setBusy] = createSignal(false);
  const off = async () => {
    setBusy(true);
    await app.run(() => app.core.request("hid.disable"));
    setBusy(false);
    props.onClose();
  };
  return (
    <Dialog
      open={props.open}
      title={t("ui.dev.hid_off_title")}
      warn
      confirm={t("ui.dev.hid_off_confirm")}
      cancel={t("modal.cancel")}
      busy={busy()}
      onConfirm={() => void off()}
      onCancel={() => props.onClose()}
    >
      <p>{t("ui.dev.hid_off_text")}</p>
    </Dialog>
  );
}

/** The self-test of the path to Windows failed (at a start: a warning only) */
export function hidTestFailed(snap: { hid_health?: { state: string } | null } | null | undefined): boolean {
  return snap?.hid_health?.state === "failed";
}

/** While it is on: over every screen, not dismissable */
export function HidNotice() {
  const app = useApp();
  const [asking, setAsking] = createSignal(false);
  return (
    <Show when={hidOn(app.core.state.snap?.settings)}>
      <Banner
        kind="warn"
        title={t("ui.dev.hid_banner_title")}
        actions={<Button onClick={() => setAsking(true)}>{t("ui.dev.hid_off_confirm")}</Button>}
      >
        {t(app.core.state.snap?.service_mode ? "ui.dev.hid_banner_text_service" : "ui.dev.hid_banner_text")}
        <Show when={hidTestFailed(app.core.state.snap)}>
          {" "}
          <strong data-testid="hid-test-failed">{t("ui.dev.hid_banner_test_failed")}</strong>
        </Show>
      </Banner>
      <HidOffDialog open={asking()} onClose={() => setAsking(false)} />
    </Show>
  );
}

/** Developer > App settings: the switch, and while it is on the devices */
export function HidSection() {
  const app = useApp();
  const snap = () => app.core.state.snap!;
  const on = () => hidOn(snap().settings);
  const [enabling, setEnabling] = createSignal(false);
  const [disabling, setDisabling] = createSignal(false);
  return (
    <Section title={t("ui.dev.hid_section")}>
      <Toggle
        label={t("ui.dev.hid_toggle")}
        hint={t(snap().service_mode ? "ui.dev.hid_toggle_hint_service" : "ui.dev.hid_toggle_hint")}
        checked={on()}
        controlled
        onChange={(v) => (v ? setEnabling(true) : setDisabling(true))}
      />
      <ServiceRow />
      <Show when={on()}>
        <HidDevices />
      </Show>
      <HidEnableDialog open={enabling()} onClose={() => setEnabling(false)} />
      <HidOffDialog open={disabling()} onClose={() => setDisabling(false)} />
    </Section>
  );
}

/** While a keyboard being added pairs: the number the user types on it */
export function HidPasskeyBlock(props: { passkey: HidPasskey; added: readonly HidDevice[] }) {
  const name = () => {
    const a = props.passkey.address.toUpperCase();
    const d = props.added.find((x) => x.address.toUpperCase() === a);
    return d?.name || props.passkey.address;
  };
  return (
    <div class="hid-passkey" role="status">
      <span class="device-name-text">{name()}</span>
      <span class="hid-passkey-number" data-testid="hid-passkey">
        {props.passkey.passkey}
      </span>
      <span>{t("ui.dev.hid_passkey_hint")}</span>
    </div>
  );
}

/** What the user does with the new device: from its report descriptor */
export function hidCheckHint(kind: string): string {
  if (kind === "keyboard") return t("ui.dev.hid_check_keyboard");
  if (kind === "mouse") return t("ui.dev.hid_check_mouse");
  return t("ui.dev.hid_check_any");
}

/** A device being added: the instruction and the core's countdown */
export function HidCheckBlock(props: { check: HidCheck; busy: boolean; onRemove: () => void }) {
  const [now, setNow] = createSignal(Date.now());
  // The deadline as of when the core last sent this check
  const deadline = createMemo(() => Date.now() + props.check.remaining_ms);
  const timer = setInterval(() => setNow(Date.now()), 250);
  onCleanup(() => clearInterval(timer));
  const left = () => Math.max(0, Math.ceil((deadline() - now()) / 1000));
  return (
    <div class="hid-passkey hid-check" role="status" data-testid="hid-check">
      <span class="device-name-text">{props.check.name || props.check.address}</span>
      <Show when={props.check.waiting} fallback={<span>{t("status.connecting")}</span>}>
        <span>{hidCheckHint(props.check.kind)}</span>
        <span class="muted" data-testid="hid-check-left">
          {t("ui.dev.hid_check_left", left())}
        </span>
      </Show>
      <span class="hid-device-actions">
        <Button kind="subtle" busy={props.busy} onClick={() => props.onRemove()}>
          {t("ui.device.forget")}
        </Button>
      </span>
    </div>
  );
}

/** A device removed again: one sentence, Try again and Close */
export function HidRolledBackBlock(props: {
  rolledBack: HidRolledBack;
  busy: boolean;
  onRetry: () => void;
  onClose: () => void;
}) {
  return (
    <div class="hid-passkey hid-rolled-back" role="alert" data-testid="hid-rolled-back">
      <span>{t("ui.dev.hid_rolled_back", props.rolledBack.name || props.rolledBack.address)}</span>
      <span class="hid-device-actions">
        <Button busy={props.busy} onClick={() => props.onRetry()}>
          {t("ui.fix.retry")}
        </Button>
        <Button kind="subtle" disabled={props.busy} onClick={() => props.onClose()}>
          {t("modal.close")}
        </Button>
      </span>
    </div>
  );
}

function HidDevices() {
  const app = useApp();
  const snap = () => app.core.state.snap!;
  const added = () => snap().hid_devices ?? [];
  const [counts, setCounts] = createSignal<HidCounts | null>(null);
  // One request at a time: adding pairs, the others wait
  const [working, setWorking] = createSignal<string | null>(null);
  const [removing, setRemoving] = createSignal<HidDevice | null>(null);
  const [scanned, setScanned] = createSignal(false);

  const readCounts = () =>
    void app.core.request("hid.status").then(
      (s) => setCounts(s.counts),
      () => setCounts(null),
    );
  createEffect(() => {
    readCounts();
    const timer = setInterval(readCounts, COUNTS_EVERY_MS);
    onCleanup(() => clearInterval(timer));
  });

  const scanning = () => snap().scanning;
  const scan = async () => {
    await app.run(() => app.core.request("device.scan", { seconds: SCAN_SECONDS }));
    setScanned(true);
  };
  const candidates = () => hidCandidates(snap().devices, snap().hid_candidates ?? [], added());

  const addAddress = async (address: string, name: string, le: number | null) => {
    setWorking(address);
    // Pairing needs the radio: a scan still running ends first
    if (scanning()) await app.core.request("device.scan_stop").catch(() => undefined);
    await app.run(() => app.core.request("hid.add", { address, name, le }));
    setWorking(null);
  };
  const add = (d: Device) => addAddress(d.address, d.name || d.address, d.hid_le);
  const retry = (r: HidRolledBack) => addAddress(r.address, r.name || r.address, r.le);
  const cancelCheck = async (c: HidCheck) => {
    setWorking(c.address);
    await app.run(() => app.core.request("hid.check_cancel", { address: c.address }));
    setWorking(null);
  };
  const dismiss = () => void app.run(() => app.core.request("hid.check_dismiss"));
  const connect = async (d: HidDevice) => {
    setWorking(d.address);
    await app.run(() => app.core.request("hid.connect", { address: d.address }));
    setWorking(null);
  };
  const remove = async () => {
    const d = removing();
    if (!d) return;
    setWorking(d.address);
    await app.run(() => app.core.request("hid.remove", { address: d.address }));
    setWorking(null);
    setRemoving(null);
  };

  return (
    <div class="hid-devices">
      <Show when={snap().hid_passkey}>{(p) => <HidPasskeyBlock passkey={p()} added={added()} />}</Show>
      <For each={snap().hid_checks ?? []}>
        {(c) => (
          <HidCheckBlock check={c} busy={working() === c.address} onRemove={() => void cancelCheck(c)} />
        )}
      </For>
      <Show when={snap().hid_rolled_back}>
        {(r) => (
          <HidRolledBackBlock
            rolledBack={r()}
            busy={working() === r().address}
            onRetry={() => void retry(r())}
            onClose={dismiss}
          />
        )}
      </Show>
      <Show when={added().length > 0} fallback={<p class="hint">{t("ui.dev.hid_devices_empty")}</p>}>
        <ul class="hid-list">
          <For each={added()}>
            {(d) => (
              <li class="row hid-device">
                <Icon name="bluetooth" />
                <span class="check-item-text">
                  <span class="device-name-text">{d.name || d.address}</span>
                  <span class="muted">{d.connected ? t("status.connected") : t("ui.dev.hid_not_connected")}</span>
                </span>
                <span class="hid-device-actions">
                  <Show when={!d.connected}>
                    <Button
                      busy={working() === d.address}
                      disabled={working() !== null}
                      onClick={() => void connect(d)}
                    >
                      {t("connection.connect")}
                    </Button>
                  </Show>
                  <Button kind="subtle" disabled={working() !== null} onClick={() => setRemoving(d)}>
                    {t("ui.device.forget")}
                  </Button>
                </span>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <div class="inline">
        <Show
          when={scanning()}
          fallback={
            <Button icon="search" disabled={working() !== null} onClick={() => void scan()}>
              {t("connection.scan")}
            </Button>
          }
        >
          <Button onClick={() => void app.run(() => app.core.request("device.scan_stop"))}>
            {t("connection.scan_stop")}
          </Button>
        </Show>
        <span class="hint">{t("ui.dev.hid_pairing_mode")}</span>
      </div>
      <Show when={candidates().length > 0}>
        <ul class="hid-list">
          <For each={candidates()}>
            {(d) => (
              <li class="row hid-device">
                <Icon name="bluetooth" />
                <span class="check-item-text">
                  <span class="device-name-text">{d.name || d.address}</span>
                  <Show when={d.name}>
                    <span class="mono muted">{d.address}</span>
                  </Show>
                </span>
                <span class="hid-device-actions">
                  <Button
                    busy={working() === d.address}
                    disabled={working() !== null}
                    onClick={() => void add(d)}
                  >
                    {t("ui.devices.add")}
                  </Button>
                </span>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={scanned() && !scanning() && candidates().length === 0}>
        <p class="hint">{t("ui.dev.hid_found_none")}</p>
      </Show>
      <Show when={counts()}>
        {(c) => (
          <p class="hint mono hid-counts">{t("ui.dev.hid_counts", c().reports, c().injected, c().blocked)}</p>
        )}
      </Show>
      <Dialog
        open={removing() !== null}
        title={t("ui.dev.hid_remove_title", removing()?.name || removing()?.address || "")}
        warn
        confirm={t("ui.device.forget")}
        cancel={t("modal.cancel")}
        busy={working() !== null}
        onConfirm={() => void remove()}
        onCancel={() => setRemoving(null)}
      >
        <p>{t("ui.dev.hid_remove_text")}</p>
      </Dialog>
    </div>
  );
}
