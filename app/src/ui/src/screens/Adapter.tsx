// Adapter: the USB Bluetooth adapters with their driver and firmware,
// WinUSB assign / restore, the chip for the firmware, and AFH.

import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  type JSX,
  Match,
  onCleanup,
  onMount,
  Show,
  Switch,
} from "solid-js";
import { useApp } from "../app/context";
import { saveSettings } from "../app/settings";
import { Banner, Button, Field, Section, Segmented, Select, Toggle } from "../components/controls";
import { Icon } from "../components/Icon";
import { ErrorPanel, FirmwareDownload, FirmwareFolderButton, WinusbDialog } from "../components/stream";
import { t, tm } from "../i18n";
import type { AdapterInfo, AdapterState, DriverKind, FirmwareFile, ProbeResult } from "../ipc/types";
import {
  adapterFirmwareTag,
  buildChipOptions,
  CHIP_NOT_REALTEK,
  type ChipStatus,
  chipStatus,
  chipTargetIndex,
  isAutoChip,
  loaderOutcome,
  vendorFirmware,
} from "../lib/chips";
import { chipReportUrl, LINKS, vendorFirmwareUrl } from "../lib/links";
import { AdapterHung, hiddenByHung } from "./Home";

/** The adapters BlueCaster can use first (WinUSB), the ones without a driver
 *  and then the ones it cannot use at all last */
export function usableFirst(list: readonly AdapterInfo[]): AdapterInfo[] {
  const rank = (a: AdapterInfo) =>
    (adapterSupported(a) ? 0 : 3) + (a.driver === "winusb" ? 0 : a.driver === "none" ? 2 : 1);
  return [...list].sort((a, b) => rank(a) - rank(b));
}

const hex4 = (n: number) => n.toString(16).toUpperCase().padStart(4, "0");

/** The adapter BlueCaster has open (AdapterState::active, which the core
 *  takes from this list): the listed one with its VID:PID on WinUSB */
export function isActiveAdapter(
  a: Pick<AdapterInfo, "vid" | "pid" | "driver">,
  active: AdapterState["active"],
): boolean {
  return !!active && a.vid === active.vid && a.pid === active.pid && a.driver === "winusb";
}

/** Settings::adapter_id names this adapter (any case) */
export function isChosenAdapter(a: Pick<AdapterInfo, "instance_id">, chosen: string): boolean {
  return chosen.trim() !== "" && a.instance_id.toLowerCase() === chosen.trim().toLowerCase();
}

/** The adapter button on a card on WinUSB: "auto" on the chosen one (back
 *  to the first one found), "use" on the others but the one in use while
 *  none is chosen, none on the rest */
export function adapterUseButton(
  a: Pick<AdapterInfo, "instance_id" | "vid" | "pid" | "driver">,
  chosen: string,
  active: AdapterState["active"],
): "use" | "auto" | null {
  if (a.driver !== "winusb") return null;
  if (isChosenAdapter(a, chosen)) return "auto";
  if (chosen.trim() === "" && isActiveAdapter(a, active)) return null;
  return "use";
}

/** A chosen adapter that is not on WinUSB (unplugged, or its driver changed) */
export function chosenMissing(list: readonly AdapterInfo[], chosen: string): boolean {
  return chosen.trim() !== "" && !list.some((a) => a.driver === "winusb" && isChosenAdapter(a, chosen));
}

/** "Realtek Bluetooth 5.3 Adapter (RTL8761BU)": the name Device Manager
 *  gives it with the chip (issue #86); before Windows' Bluetooth ever ran
 *  it and with no product name of its own, the vendor and chip ("Realtek
 *  RTL8761BU"), without the VID:PID the line below shows and the
 *  "[unsupported]" tag says */
export function adapterName(
  a: Pick<AdapterInfo, "display_name" | "vendor" | "windows_name" | "realtek_chip">,
): string {
  const name = a.windows_name?.trim();
  if (!name) {
    const chip = a.display_name
      .replace(/\s*\(0x[0-9a-f]{4}:0x[0-9a-f]{4}\)/i, "")
      .replace(/\s*\[unsupported\]/i, "")
      .trim();
    if (!a.vendor || chip.toLowerCase().startsWith(a.vendor.toLowerCase())) return chip;
    return `${a.vendor} ${chip}`;
  }
  const chip = a.realtek_chip;
  return chip && !name.toUpperCase().includes(chip.toUpperCase()) ? `${name} (${chip})` : name;
}

/** "8C:88:4B:46:DD:AE | 0BDA:A729 | v5.1": what is known of it */
export function adapterIds(a: Pick<AdapterInfo, "address" | "vid" | "pid" | "bt_version">): string {
  return [a.address, `${hex4(a.vid)}:${hex4(a.pid)}`, a.bt_version && `v${a.bt_version}`].filter(Boolean).join(" | ");
}

/** Why adapterIds lacks the address or the Bluetooth version: neither
 *  Windows' Bluetooth (it runs one radio only) nor BlueCaster has
 *  used the adapter yet (issue #86); undefined when both are known */
export function adapterIdsHint(a: Pick<AdapterInfo, "address" | "bt_version">): string | undefined {
  return a.address && a.bt_version ? undefined : t("ui.adapter.ids_unknown");
}

/** BlueCaster can stream through it: not a chip whose firmware loader it lacks
 *  (MediaTek, Qualcomm). Those need their firmware loaded at every start
 *  before they answer as a Bluetooth controller, so they cannot work at all:
 *  neither WinUSB nor a firmware download is offered for them (issue #86).
 *  Developer mode offers WinUSB anyway, for the chip probe */
export function adapterSupported(a: Pick<AdapterInfo, "vendor_note">): boolean {
  return a.vendor_note?.key !== "firmware.vendor_unsupported";
}

/** The driver the adapter has now, in two names only whatever the adapter
 *  and its manufacturer (issue #86): WinUSB, or the "original" one WinUSB
 *  replaces (Windows' BTHUSB, or a vendor's own such as MediaTek's filter or
 *  a composite device's; the service shows on hover) */
export function driverText(d: DriverKind): { text: string; status: "good" | "warn" | "bad" } {
  switch (d) {
    case "winusb":
      return { text: t("ui.adapter.driver_winusb"), status: "good" };
    case "bluetooth":
    case "other":
      return { text: t("ui.adapter.driver_original"), status: "warn" };
    default:
      return { text: t("ui.adapter.driver_none"), status: "bad" };
  }
}

/** How often the shown list is asked for anew: there is no Refresh button
 *  (issue #86) and the core has no device notification, so an adapter
 *  plugged in or out shows up by itself */
export const ADAPTER_POLL_MS = 5000;

export function Adapter() {
  const app = useApp();
  const { core } = app;
  const snap = () => core.state.snap!;
  const adapter = () => snap().adapter;
  // The order shown; the chip section goes with its adapter in it
  const sorted = () => usableFirst(adapter().adapters);
  // The adapter to use (Settings::adapter_id): a button on each card, once
  // two are on WinUSB or while one is chosen
  const chosen = () => snap().settings.adapter_id ?? "";
  const showChoice = () =>
    adapter().adapters.filter((a) => a.driver === "winusb").length > 1 || chosen().trim() !== "";
  const choose = (adapter_id: string) => void saveSettings(app, { adapter_id });
  // The card the chip choice goes in; none for an adapter BlueCaster cannot
  // use, the choice then stands on its own below the cards
  const chipCard = () => {
    const i = chipTargetIndex(sorted());
    return i >= 0 && adapterSupported(sorted()[i]) ? i : -1;
  };
  const [dialog, setDialog] = createSignal<{ adapter: AdapterInfo; mode: "assign" | "restore" } | null>(null);
  const [listing, setListing] = createSignal(false);
  // Developer mode: WinUSB also for a chip BlueCaster cannot use yet, and the
  // chip probe (read commands only) for whoever adds its loader
  const developer = () => snap().settings.developer_mode;
  const [probing, setProbing] = createSignal(false);
  // The last probe of each adapter (instance ID): its summary and the
  // report to send stay on the card
  const [probed, setProbed] = createSignal<Record<string, ProbeResult>>({});
  const probe = (a: AdapterInfo) => {
    setProbing(true);
    void app
      .run(async () => {
        const r = await core.request("adapter.probe", { instance_id: a.instance_id });
        setProbed((p) => ({ ...p, [a.instance_id]: r }));
        app.notify(t("ui.adapter.probe_saved", r.summary));
        await core.transport.revealPath(r.path);
      })
      .finally(() => setProbing(false));
  };
  // Developer mode: the keys Windows kept for this adapter's devices
  /** A prefilled chip report in the browser, with the probe report in it;
   *  one too long for the URL is shown in Explorer to drag into it */
  const reportChip = (a: AdapterInfo, r: ProbeResult) =>
    void app.run(async () => {
      const info = (await core.request("diag.info", { mask: true })).text;
      const { url, attach } = chipReportUrl({
        version: snap().update.current_version,
        adapter: a.display_name,
        summary: r.summary,
        info,
        report: r.report ?? "",
      });
      if (attach) await core.transport.revealPath(r.path);
      await core.transport.openUrl(url);
    });
  // The *_fw.bin files in the settings folder: chips the table lacks
  const [files, setFiles] = createSignal<FirmwareFile[]>([]);
  const loadFiles = async () => {
    try {
      setFiles(await core.request("adapter.firmware_files"));
    } catch {
      // The choice keeps the table's chips
    }
  };

  // The list anew, one request at a time
  const relist = async () => {
    if (listing()) return;
    setListing(true);
    await Promise.all([core.request("adapter.list").catch(() => {}), loadFiles()]);
    setListing(false);
  };

  // v1 checked the firmware again when its dialog got the focus back: the
  // files may have been placed through Open Config Folder meanwhile. On
  // opening the screen and every few seconds while it is in view as well,
  // for an adapter plugged in or out
  const onFocus = () => void relist();
  onMount(() => {
    void relist();
    window.addEventListener("focus", onFocus);
    const timer = setInterval(() => {
      if (document.visibilityState === "visible" && dialog() === null) void relist();
    }, ADAPTER_POLL_MS);
    onCleanup(() => clearInterval(timer));
  });
  onCleanup(() => window.removeEventListener("focus", onFocus));
  // #/adapter/afh (the receiver test links there): the AFH section in view;
  // #/adapter/firmware (Home's firmware bar): the firmware part
  createEffect(() => {
    const id = { afh: AFH_SECTION, firmware: FIRMWARE_SECTION }[app.route().arg ?? ""];
    if (id) document.getElementById(id)?.scrollIntoView?.({ block: "start" });
  });

  return (
    <div class="page">
      <header class="page-head">
        <h1>{t("ui.nav.adapter")}</h1>
        <div class="head-actions">
          <Button icon="arrow_reset" onClick={() => app.run(() => core.request("adapter.restart"))}>
            {t("ui.adapter.restart")}
          </Button>
        </div>
      </header>

      <p class={`status-line st-${adapter().status}`} role="status">
        <span class="dot" aria-hidden="true" />
        {t("ui.adapter.stack")}: {t(`ui.adapter.status_${adapter().status}`)}
        {/* Which adapter it has open */}
        <Show when={adapter().active}>
          {(a) => (
            <>
              {" · "}
              {t("ui.adapter.active_line", a().windows_name || a().display_name)}
            </>
          )}
        </Show>
      </p>
      <Show when={chosenMissing(adapter().adapters, chosen())}>
        <Banner
          kind="warn"
          title={t("ui.adapter.use_missing")}
          actions={
            <Button icon="arrow_reset" onClick={() => choose("")}>
              {t("ui.adapter.use_auto")}
            </Button>
          }
        />
      </Show>
      {/* A firmware loader while the adapter starts (Intel's download), and
          how it ended: "ready" and its steps in developer mode only */}
      <Show when={adapter().loader && (developer() || adapter().loader!.ok !== true) && adapter().loader}>
        {(l) => (
          <p class={`status-line st-${l().ok === false ? "error" : l().ok ? "ready" : "initializing"}`} role="status">
            <span class="dot" aria-hidden="true" />
            {l().ok === false
              ? t("ui.adapter.loader_failed", l().vendor)
              : l().ok
                ? t("ui.adapter.loader_ok", l().vendor)
                : t("ui.adapter.loader_running", l().vendor)}
            {l().ok == null && l().percent != null ? ` ${l().percent}%` : ""}
            <Show when={developer() && l().step}>
              <span class="muted mono"> ({l().step})</span>
            </Show>
          </p>
        )}
      </Show>
      <AdapterHung />
      <Show when={adapter().status === "error" && !hiddenByHung(snap(), adapter().error) && adapter().error}>
        {(m) => <ErrorPanel message={m()} />}
      </Show>

      <Show
        when={adapter().adapters.length > 0}
        fallback={
          <Show when={!adapter().hung}>
            <Banner kind="warn" title={t("firmware.no_adapters")}>
              {t("ui.adapter.none_hint")}
            </Banner>
          </Show>
        }
      >
        <For each={sorted()}>
          {(a, i) => {
            const drv = () => driverText(a.driver);
            const fw = () => adapterFirmwareTag(a, snap().settings, adapter().loader);
            // Another vendor's adapter: where its firmware comes from, in
            // place of the Realtek chip choice (issue #86) while that is
            // Auto or Not Realtek
            const vendorBox = () =>
              vendorFirmware(a) !== null &&
              (isAutoChip(snap().settings) || snap().settings.bt_chip_pid === CHIP_NOT_REALTEK);
            // Realtek's Windows driver has the firmware (issue #86): nothing
            // to do, as for MediaTek. A file in the firmware folder still
            // takes priority, and the chip choice is back once it is set
            // by hand or the firmware did not load.
            const driverFirmware = () =>
              a.firmware_from_driver && isAutoChip(snap().settings) && !adapter().rom_firmware;
            // The firmware folder's files although the driver has the
            // firmware: back to the driver's
            const customFirmware = () => a.firmware_driver && snap().settings.realtek_custom_firmware;
            const setCustom = (on: boolean) => void saveSettings(app, { realtek_custom_firmware: on });
            const driverHead = createUniqueId();
            const firmwareHead = createUniqueId();
            return (
              <section
                class="card adapter-card"
                classList={{ "adapter-active": isActiveAdapter(a, adapter().active) }}
                aria-label={adapterName(a)}
              >
                <div class="adapter-head">
                  <Icon name="usb_stick" />
                  <div>
                    {/* Device Manager's name; the USB device's own on hover */}
                    <h2 title={a.device_name}>
                      {adapterName(a)}
                      <Show when={isActiveAdapter(a, adapter().active)}>
                        {" "}
                        <span class="tag st-good">{t("ui.adapter.active")}</span>
                      </Show>
                      <Show when={isChosenAdapter(a, chosen()) && !isActiveAdapter(a, adapter().active)}>
                        {" "}
                        <span class="tag st-warn" title={t("ui.adapter.chosen_hint")}>
                          {t("ui.adapter.chosen")}
                        </span>
                      </Show>
                    </h2>
                    <span class="muted mono" title={adapterIdsHint(a)}>
                      {adapterIds(a)}
                    </span>
                  </div>
                  {/* Which adapter BlueCaster opens */}
                  <Show when={showChoice() && adapterUseButton(a, chosen(), adapter().active)}>
                    {(b) => (
                      <div class="adapter-use">
                        <Show
                          when={b() === "use"}
                          fallback={
                            <Button
                              icon="arrow_reset"
                              title={t("ui.adapter.use_auto_hint")}
                              onClick={() => choose("")}
                            >
                              {t("ui.adapter.use_auto")}
                            </Button>
                          }
                        >
                          <Button
                            kind="strong"
                            icon="checkmark"
                            title={t("ui.adapter.use_hint")}
                            onClick={() => choose(a.instance_id)}
                          >
                            {t("ui.adapter.use")}
                          </Button>
                        </Show>
                      </div>
                    )}
                  </Show>
                </div>
                <div class="adapter-boxes">
                  <div class="adapter-box" role="group" aria-labelledby={driverHead}>
                    <h3 id={driverHead}>
                      {t("ui.adapter.driver")}:{" "}
                      <span class={`tag st-${drv().status}`} title={a.driver_service || undefined}>
                        {drv().text}
                      </span>
                      <Show when={a.restart_needed}>
                        {" "}
                        <span class="tag st-warn" title={t("ui.adapter.restart_needed_hint")}>
                          {t("ui.adapter.restart_needed")}
                        </span>
                      </Show>
                    </h3>
                    <Show
                      when={a.driver === "winusb"}
                      fallback={
                        <>
                          {/* One BlueCaster cannot use keeps its driver (issue #86) */}
                          <Button
                            kind="strong"
                            icon="shield"
                            disabled={!adapterSupported(a) && !developer()}
                            onClick={() => setDialog({ adapter: a, mode: "assign" })}
                          >
                            {t("ui.adapter.assign_winusb")}
                          </Button>
                          <p class="hint">
                            {t(
                              adapterSupported(a) || developer()
                                ? "ui.adapter.assign_short"
                                : "ui.adapter.assign_unsupported",
                            )}
                          </p>
                        </>
                      }
                    >
                      <Button icon="shield" onClick={() => setDialog({ adapter: a, mode: "restore" })}>
                        {t("ui.adapter.restore_driver")}
                      </Button>
                      <p class="hint">{t("ui.adapter.restore_short")}</p>
                    </Show>
                  </div>
                  <div
                    class="adapter-box"
                    id={i() === chipCard() ? FIRMWARE_SECTION : undefined}
                    role="group"
                    aria-labelledby={firmwareHead}
                  >
                    <h3 id={firmwareHead}>
                      {t("ui.adapter.firmware")}:{" "}
                      <span class={`tag st-${fw().status}`}>{t(fw().key, ...(fw().args ?? []))}</span>
                      {/* The running stack found it on its ROM code (issue #81) */}
                      <Show when={adapter().rom_firmware && a.realtek_chip && a.driver === "winusb"}>
                        {" "}
                        <span class="tag st-bad" title={t("firmware.rom_status")}>
                          {t("ui.adapter.rom_firmware")}
                        </span>
                      </Show>
                    </h3>
                    {/* The chip setting is one for all adapters: with the one it applies to */}
                    <Show when={!driverFirmware()}>
                      <Show
                        when={vendorBox()}
                        fallback={
                          <Show when={i() === chipCard()}>
                            <ChipSection files={files()} onChanged={loadFiles} adapter={a} />
                          </Show>
                        }
                      >
                        <VendorFirmwareBox adapter={a} />
                      </Show>
                    </Show>
                    {/* Issue #86: other firmware (Linux's) for those who want it */}
                    <Show when={driverFirmware()}>
                      <Button icon="wrench" onClick={() => setCustom(true)}>
                        {t("ui.adapter.custom_firmware")}
                      </Button>
                    </Show>
                    <Show when={customFirmware()}>
                      <Button icon="arrow_reset" onClick={() => setCustom(false)}>
                        {t("ui.adapter.driver_firmware")}
                      </Button>
                    </Show>
                    <Show when={!adapterSupported(a)}>
                      <Button icon="arrow_download" disabled>
                        {t("ui.adapter.firmware_download_unavailable")}
                      </Button>
                      <p class="hint">{t("firmware.vendor_unsupported")}</p>
                    </Show>
                    <Show when={developer() && a.driver === "winusb"}>
                      <Button icon="search" disabled={probing()} onClick={() => probe(a)}>
                        {t("ui.adapter.probe")}
                      </Button>
                      <Show when={probed()[a.instance_id]}>
                        {(r) => (
                          <>
                            <p class="hint">{r().summary}</p>
                            <Button icon="open" onClick={() => reportChip(a, r())}>
                              {t("ui.adapter.probe_report")}
                            </Button>
                          </>
                        )}
                      </Show>
                    </Show>
                  </div>
                </div>
              </section>
            );
          }}
        </For>
      </Show>

      {/* No adapter to put it with: on its own, to choose the chip beforehand */}
      <Show when={chipCard() < 0}>
        <ChipSection files={files()} onChanged={loadFiles} />
      </Show>
      <LeAudioToggle />
      <AfhSection />

      <WinusbDialog adapter={dialog()?.adapter ?? null} mode={dialog()?.mode ?? "assign"} onClose={() => setDialog(null)} />
    </div>
  );
}

/** v1 FirmwareDialog: the chip choice and the firmware state of that chip */
function ChipSection(props: { files: FirmwareFile[]; onChanged: () => Promise<void>; adapter?: AdapterInfo }) {
  const app = useApp();
  const snap = () => app.core.state.snap!;
  const settings = () => snap().settings;
  const adapters = () => snap().adapter.adapters;
  const detected = () => adapters().find((a) => a.realtek_chip)?.realtek_chip ?? null;
  const choice = () =>
    buildChipOptions(settings(), props.files, {
      auto: t("firmware.chip_auto", detected() ?? t("firmware.chip_auto_none")),
      nonRealtek: t("firmware.chip_non_realtek"),
    });
  const hint = () => (isAutoChip(settings()) && detected() ? undefined : t("firmware.select_chip"));
  const choose = async (key: string) => {
    const o = choice().options.find((x) => x.key === key);
    if (!o || (o.pid === settings().bt_chip_pid && o.stem === settings().bt_chip_fw_stem)) return;
    // The core lists the adapters anew with the firmware state for this chip
    // (and releases an idle stack); the folder may have changed as well
    if (await saveSettings(app, { bt_chip_pid: o.pid, bt_chip_fw_stem: o.stem })) await props.onChanged();
  };
  // A new object on every published state would build the banner anew and
  // close a firmware download's dialog while it runs: only a changed status
  // counts
  const status = createMemo(() => chipStatus(settings(), adapters(), props.files), undefined, {
    equals: (a, b) => JSON.stringify(a) === JSON.stringify(b),
  });
  const body = () => (
    <>
      <Field label={t("settings.bt_chip")} hint={hint()} stacked>
        {(id, described) => (
          <Select
            id={id}
            describedBy={described}
            value={choice().selected}
            options={choice().options.map((o) => ({ value: o.key, label: o.label }))}
            onChange={(key) => void choose(key)}
          />
        )}
      </Field>
      <div aria-live="polite">
        <Show when={status()} keyed>
          {(st) => <FirmwareStatus status={st} embedded={props.adapter !== undefined} />}
        </Show>
      </div>
      <div class="inline wrap">
        {/* Also to fetch the files again when they are there */}
        <Show when={props.adapter?.firmware_download && props.adapter}>
          {(a) => <FirmwareDownload files={a().firmware_files} kind={a().firmware_present ? undefined : "strong"} />}
        </Show>
        <FirmwareFolderButton kind="subtle" />
        <Button icon="open" kind="subtle" onClick={() => app.core.transport.openUrl(LINKS.setupGuide)}>
          {t("ui.setup.guide")}
        </Button>
      </div>
    </>
  );
  // In the adapter's firmware box, which has the heading
  return (
    <Show
      when={props.adapter}
      fallback={
        <Section id={FIRMWARE_SECTION} title={t("firmware.title")}>
          {body()}
        </Section>
      }
    >
      {body()}
    </Show>
  );
}

/** Another vendor's adapter (issue #86): what to do about its firmware, and
 *  only where there is something to do. Its vendor's driver or none needed:
 *  nothing, unless loading failed; Intel's files (and Broadcom's optional
 *  ones) in the firmware folder: where to get them; Marvell: Wi-Fi on */
function VendorFirmwareBox(props: { adapter: AdapterInfo }) {
  const app = useApp();
  const a = () => props.adapter;
  const source = () => vendorFirmware(a());
  const outcome = () => loaderOutcome(a(), app.core.state.snap!.adapter.loader);
  const url = () => vendorFirmwareUrl(a().vendor);
  const actions = (guide: boolean) => (
    <div class="inline wrap">
      <Show when={url()}>
        {(u) => (
          <Button icon="open" onClick={() => app.core.transport.openUrl(u())}>
            {t("firmware.open_download_page")}
          </Button>
        )}
      </Show>
      <FirmwareFolderButton kind="subtle" />
      <Show when={guide}>
        <Button icon="open" kind="subtle" onClick={() => app.core.transport.openUrl(LINKS.setupGuide)}>
          {t("ui.setup.guide")}
        </Button>
      </Show>
    </div>
  );
  return (
    <Switch>
      <Match when={outcome() === "failed"}>
        <p class="hint">
          {source() === "driver" ? t("firmware.from_vendor_driver", a().vendor) : t("firmware.intel_files")}
        </p>
        {actions(source() === "folder")}
      </Match>
      <Match when={source() === "folder" && outcome() !== "ok"}>
        <p class="hint">{t("firmware.intel_files")}</p>
        {actions(true)}
      </Match>
      <Match when={source() === "optional"}>{actions(false)}</Match>
      <Match when={source() === "wifi"}>
        <p class="hint">{t("ui.adapter.firmware_keep_wifi")}</p>
      </Match>
    </Switch>
  );
}

/** v1 UpdateFirmwareStatus: the files, what is missing, a vendor's note */
/** `embedded`: in the adapter's firmware box, whose tag says the files are
 *  there and which has the download button itself */
function FirmwareStatus(props: { status: ChipStatus; embedded?: boolean }): JSX.Element {
  const app = useApp();
  const download = (url: string) => (
    <Button icon="open" onClick={() => app.core.transport.openUrl(url)}>
      {t("firmware.open_download_page")}
    </Button>
  );
  // The core fetches the files itself; the page stays for doing it by hand
  const realtekActions = (files: string[], fetch: boolean) => (
    <>
      <Show when={fetch && !props.embedded}>
        <FirmwareDownload files={files} kind="strong" />
      </Show>
      {download(LINKS.firmwareRealtek)}
    </>
  );
  const fileList = (files: string[]) => (
    <ul class="mono files">
      <For each={files}>{(f) => <li>{f}</li>}</For>
    </ul>
  );
  const st = props.status;
  switch (st.kind) {
    case "present":
      if (props.embedded) return null;
      return (
        <Banner kind="good" title={t("firmware.present")}>
          {fileList(st.files)}
        </Banner>
      );
    case "missing":
    case "unchecked": {
      if (st.kind === "unchecked" && st.present === true) {
        if (props.embedded) return null;
        return (
          <Banner kind="good" title={t("firmware.present")}>
            {fileList(st.files)}
          </Banner>
        );
      }
      const missing = st.kind === "missing" || st.present === false;
      return (
        <Banner
          kind={missing ? "warn" : "info"}
          title={missing ? t("firmware.missing_manual") : undefined}
          actions={realtekActions(st.files, st.kind === "missing" && st.download)}
        >
          <p>{t("firmware.required_files")}</p>
          {fileList(st.files)}
          <p class="hint">{t("firmware.manual_instructions")}</p>
        </Banner>
      );
    }
    case "invalid":
      return (
        <Banner kind="warn" title={t("firmware.invalid_short")} actions={realtekActions(st.files, st.download)}>
          <p>{t("firmware.invalid", st.file)}</p>
          <p>{t("firmware.required_files")}</p>
          {fileList(st.files)}
        </Banner>
      );
    case "vendor": {
      // Per chip: a vendor's chips without a loader get no firmware page,
      // even when its other chips have one
      const unsupported = st.note.key === "firmware.vendor_unsupported";
      const url = unsupported ? null : vendorFirmwareUrl(st.vendor);
      return (
        <Banner
          kind={unsupported ? "warn" : "info"}
          actions={url ? download(url) : undefined}
        >
          {tm(st.note)}
        </Banner>
      );
    }
    case "non_realtek":
      return <Banner kind="good">{t("firmware.non_realtek")}</Banner>;
    case "auto_no_adapter":
      // Not "bind it with Zadig" while the adapter seems hung (AdapterHung says what to do)
      return (
        <Show when={!app.core.state.snap?.adapter.hung}>
          <Banner kind="warn">{t("firmware.auto_no_adapter")}</Banner>
        </Show>
      );
    case "auto_unrecognized":
      return <Banner kind="warn">{t("firmware.auto_unrecognized")}</Banner>;
    default:
      return <Banner kind="warn">{t("firmware.select_chip")}</Banner>;
  }
}

/** "6,11": Wi-Fi channels 1..14, at least one */
export function parseAfhChannels(s: string): string | null {
  const parts = s.split(/[\s,]+/).filter(Boolean);
  if (parts.length === 0) return null;
  const nums = parts.map(Number);
  if (nums.some((n) => !Number.isInteger(n) || n < 1 || n > 14)) return null;
  return [...new Set(nums)].sort((a, b) => a - b).join(",");
}

/** LE Audio / BLE stack toggle in developer mode (also needed for plain BLE
 *  GATT controls like QCY earbuds on adapters without ISO channels) */
export function leAudioToggleShown(
  _adapter: { le_audio: boolean | null; adapters: readonly { le_audio: boolean | null }[] },
  developer: boolean,
): boolean {
  return developer;
}

function LeAudioToggle() {
  const app = useApp();
  const snap = () => app.core.state.snap!;
  return (
    <Show when={leAudioToggleShown(snap().adapter, snap().settings.developer_mode)}>
      <section class="card section">
        <Toggle
          label={t("ui.adapter.le_audio")}
          checked={snap().settings.le_audio_experimental}
          onChange={(v) => void saveSettings(app, { le_audio_experimental: v })}
        />
      </section>
    </Show>
  );
}

const AFH_SECTION = "adapter-afh";
const FIRMWARE_SECTION = "adapter-firmware";

function AfhSection() {
  const app = useApp();
  const afh = () => app.core.state.snap!.settings.afh;
  const developer = () => app.core.state.snap!.settings.developer_mode;
  // "blend-mild" / "-medium" / "-strong": experimental, developer mode
  // only; without it the stack runs Auto, and so it shows
  const strictness = () => (afh().startsWith("blend-") ? afh().slice("blend-".length) : null);
  const mode = () =>
    afh() === "auto" || afh() === "off" ? afh() : strictness() ? (developer() ? "blend" : "auto") : "manual";
  const [channels, setChannels] = createSignal(mode() === "manual" ? afh() : "");
  const [manual, setManual] = createSignal(mode() === "manual");
  const bad = () => manual() && channels().trim() !== "" && parseAfhChannels(channels()) === null;
  const saveChannels = () => {
    const c = parseAfhChannels(channels());
    if (c && c !== afh()) void saveSettings(app, { afh: c });
  };
  // Auto: the Wi-Fi channels the running stack avoids now (empty while none
  // or no stack runs)
  onCleanup(app.core.wantLink());
  const avoided = () => {
    const l = app.core.link();
    return l.length ? l[l.length - 1].afh_avoided.replace(/\s+/g, "") : "";
  };
  // One box for the channels (issue #86): Auto shows what it chose, Off
  // nothing, both read only; Manual takes the user's
  const shown = () => (manual() ? channels() : mode() === "auto" || mode() === "blend" ? avoided() : "");
  return (
    <Section id={AFH_SECTION} title={t("receiver.afh")}>
      {/* The text in a block of its own, the controls beside it (issue #86) */}
      <div class="afh-body">
        <div class="afh-text">
          <p class="hint">{t("receiver.afh_tooltip")}</p>
          <Show when={bad()}>
            <span class="field-error" id="afh-channels-err" role="alert">
              <Icon name="error_circle" />
              {t("ui.adapter.afh_bad")}
            </span>
          </Show>
        </div>
        <div class="afh-control">
          <div class="afh-row">
            <Segmented
              label={t("receiver.afh")}
              value={manual() ? "manual" : mode()}
              options={[
                { value: "auto", label: t("receiver.afh_auto") },
                { value: "off", label: t("receiver.afh_off") },
                { value: "manual", label: t("receiver.afh_manual") },
                ...(developer() ? [{ value: "blend", label: t("receiver.afh_blend") }] : []),
              ]}
              onChange={(v) => {
                setManual(v === "manual");
                if (v === "manual") saveChannels();
                else void saveSettings(app, { afh: v === "blend" ? "blend-medium" : v });
              }}
            />
            <input
              class="input mono afh-channels"
              placeholder={manual() ? "6,11" : ""}
              value={shown()}
              readOnly={!manual()}
              aria-label={t("receiver.afh_channels")}
              aria-describedby="afh-channels-err"
              aria-invalid={bad() ? "true" : undefined}
              onInput={(e) => setChannels(e.currentTarget.value)}
              onBlur={() => manual() && saveChannels()}
              onKeyDown={(e) => manual() && e.key === "Enter" && saveChannels()}
            />
          </div>
          {/* The strength on a row of its own, below the mode (issue #86) */}
          <Show when={!manual() && mode() === "blend"}>
            <Segmented
              label={t("receiver.afh_strictness")}
              value={strictness() ?? "medium"}
              options={[
                { value: "mild", label: t("receiver.afh_mild") },
                { value: "medium", label: t("receiver.afh_medium") },
                { value: "strong", label: t("receiver.afh_strong") },
              ]}
              onChange={(v) => void saveSettings(app, { afh: `blend-${v}` })}
            />
          </Show>
        </div>
      </div>
    </Section>
  );
}
