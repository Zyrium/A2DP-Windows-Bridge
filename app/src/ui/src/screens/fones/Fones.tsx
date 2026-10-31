// The Fones screen: control of the QCY headset over BLE (phase 4, UI
// only). Talks to a QcyProvider (ipc/qcy.ts); the audio stream is the
// core's and is only shown here (codec badge).

import { createEffect, createResource, createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import { useApp, errorText } from "../../app/context";
import { Banner, Button, Dialog, Section, Segmented, Select, Toggle, type Option } from "../../components/controls";
import { Icon } from "../../components/Icon";
import { CodecBadge } from "../../components/stream";
import { createQcy, EQ_BANDS_HZ, EQ_GAIN_MAX, EQ_GAIN_MIN, TOUCH_ACTIONS, type AutoOff, type Battery, type GestureKind, type NoiseMode, type NoiseScene, type QcyChange, type QcyProvider, type QcyState, type QcyWrite, type Side } from "../../ipc/qcy";
import { insideTauri } from "../../ipc/tauri";
import { EQ_PRESETS, matchPreset } from "../../lib/eqPresets";
import { lang } from "../../i18n";
import { ft } from "./strings";
import "./fones.css";

let provider: Promise<QcyProvider> | undefined;
/** The one provider of the app (tests can replace it) */
export function qcyProvider(): Promise<QcyProvider> {
  provider ??= createQcy(insideTauri());
  return provider;
}
export function setQcyProvider(p: QcyProvider) {
  provider = Promise.resolve(p);
}

export type FonesTab = "main" | "eq" | "settings";

export function Fones(props: { tab?: string }) {
  const { navigate, notify } = useApp();
  const [qcy] = createResource(qcyProvider);
  const [state, setState] = createSignal<QcyState | null>(null);
  const [now, setNow] = createSignal(Date.now());
  const [askLdac, setAskLdac] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const tick = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(tick));

  createEffect(() => {
    const q = qcy();
    if (!q) return;
    void q.state().then(setState);
    onCleanup(q.onState(setState));
  });

  const tab = (): FonesTab => (props.tab === "eq" || props.tab === "settings" ? props.tab : "main");
  const live = () => state()?.link.state === "connected";
  const readOnly = () => state()?.read_only === true;
  /** Only these settings can change (absent: all of them) */
  const writable = () => state()?.writable;
  /** Controls usable: connected and not a read-only build */
  const editable = () => live() && !readOnly() && writable() === undefined;
  /** This one setting can be changed now */
  const can = (kind: string) => live() && !readOnly() && (writable() === undefined || writable()!.includes(kind));
  /** Some, not all, settings can change: the rest is greyed with its tag */
  const partial = () => live() && !readOnly() && (writable()?.length ?? 0) > 0;
  const reason = () => (state()?.link as { reason?: string } | undefined)?.reason;

  const act = async (f: (q: QcyProvider) => Promise<QcyState>) => {
    const q = qcy();
    if (!q) return false;
    setBusy(true);
    try {
      setState(await f(q));
      return true;
    } catch (e) {
      notify(errorText(e), "error");
      return false;
    } finally {
      setBusy(false);
    }
  };
  const send = (c: QcyChange) => act((q) => q.set(c));

  const syncText = () => {
    const s = state();
    if (!s?.synced_at) return ft("sync.never");
    const secs = Math.max(0, Math.round((now() - s.synced_at) / 1000));
    if (!live()) return ft("sync.last", Math.max(1, Math.round(secs / 60)));
    return secs < 60 ? ft("sync.ago_s", secs) : ft("sync.ago_min", Math.round(secs / 60));
  };

  return (
    <div class="page fones">
      <header class="page-head">
        <h1>{ft("title")}</h1>
        <span class="muted sync">
          <Icon name="arrow_sync" /> {syncText()}
        </span>
        <Button icon="arrow_sync" disabled={!canRefresh(state(), busy())} onClick={() => void act((q) => q.refresh())}>
          {ft("refresh")}
        </Button>
        <span class="muted refresh-hint">{ft("refresh_hint")}</span>
      </header>

      <Segmented<FonesTab>
        label={ft("title")}
        value={tab()}
        options={[
          { value: "main", label: ft("tab.main") },
          { value: "eq", label: ft("tab.eq") },
          { value: "settings", label: ft("tab.settings") },
        ]}
        onChange={(v) => navigate({ name: "fones", arg: v === "main" ? undefined : v })}
      />

      <Show when={state()}>
        {(s) => (
          <>
            <Show when={s().link.state === "unavailable" || s().link.state === "connecting"}>
              <Banner
                kind="warn"
                title={ft("unavailable.title")}
                actions={
                  <Show when={!(s().link.state === "unavailable" && (s().link as { reason?: string }).reason === "not_implemented")}>
                    <Button kind="strong" busy={s().link.state === "connecting"} onClick={() => void act((q) => q.reconnect())}>
                      {ft("unavailable.retry")}
                    </Button>
                    <Button kind="subtle" onClick={() => navigate({ name: "diagnostics" })}>
                      {ft("unavailable.diag")}
                    </Button>
                  </Show>
                }
              >
                {reason() === "not_implemented"
                  ? ft("unavailable.not_implemented")
                  : reason() === "le_off"
                    ? ft("unavailable.le_off")
                    : reason() === "not_found"
                      ? ft("unavailable.not_found")
                      : ft("unavailable.text")}
              </Banner>
            </Show>
            <Show when={s().read_only}>
              <Banner kind="info" title={ft("read_only.title")}>
                {s().link.state === "none" ? `${ft("read_only.text")} ${ft("read_only.first")}` : ft("read_only.text")}
              </Banner>
            </Show>
            <Show when={s().writable !== undefined && !s().read_only && (s().writable?.length ?? 0) > 0}>
              <Banner kind="info" title={ft("write_test.title")}>
                {ft("write_test.text")}
              </Banner>
            </Show>
            <WriteBanner s={s()} busy={busy()} send={send} diag={() => navigate({ name: "diagnostics" })} />
            <div
              class="fones-body"
              classList={{ stale: !live(), readonly: live() && readOnly() }}
              aria-disabled={!editable() && !(tab() !== "eq" && partial())}
              inert={!editable() && !(tab() !== "eq" && partial()) ? true : undefined}
            >
              <Show when={tab() === "main"}>
                <MainTab s={s()} send={send} can={can} partial={partial()} busy={busy()} verified={s().verified ?? []} />
              </Show>
              <Show when={tab() === "eq"}>
                <EqTab s={s()} send={send} notify={notify} />
              </Show>
              <Show when={tab() === "settings"}>
                <SettingsTab
                  s={s()}
                  send={send}
                  onLdacOff={() => setAskLdac(true)}
                  save={() => void act((q) => q.saveProfile())}
                  can={can}
                  partial={partial()}
                  busy={busy()}
                  verified={s().verified ?? []}
                />
              </Show>
            </div>
          </>
        )}
      </Show>

      <Dialog
        open={askLdac()}
        title={ft("ldac_off.title")}
        confirm={ft("ldac_off.confirm")}
        cancel={ft("cancel")}
        danger
        busy={busy()}
        onCancel={() => setAskLdac(false)}
        onConfirm={async () => {
          await send({ kind: "ldac", enabled: false });
          setAskLdac(false);
        }}
      >
        <p>{ft("ldac_off.text")}</p>
      </Dialog>
    </div>
  );
}

function LastValue(props: { s: QcyState }) {
  return (
    <Show when={props.s.link.state !== "connected"}>
      <span class="lastval">{ft("last_value")}</span>
    </Show>
  );
}

function linkText(s: QcyState) {
  switch (s.link.state) {
    case "connected":
      return ft("link.connected");
    case "connecting":
      return ft("link.connecting");
    case "unavailable":
      return ft("link.unavailable");
    default:
      return ft("link.none");
  }
}

function BudIcon(props: { right?: boolean }) {
  return (
    <svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true">
      <g transform={props.right ? "scale(-1,1) translate(-24,0)" : undefined}>
        <path d="M9 3.5a5 5 0 0 1 5 5v1a4 4 0 0 1-3 3.9V20a1.5 1.5 0 0 1-3 0v-7A5 5 0 0 1 9 3.5z" />
        <circle cx="9.5" cy="8.5" r="1.6" />
      </g>
    </svg>
  );
}
function CaseIcon() {
  return (
    <svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true">
      <rect x="3" y="6" width="18" height="12.5" rx="6" />
      <path d="M3.3 10.5h17.4" />
      <circle cx="12" cy="15" r=".9" fill="currentColor" />
    </svg>
  );
}

function BatteryRing(props: { b: Battery; label: string; icon: JSX.Element }) {
  const color = () => ((props.b.level ?? 0) <= 15 ? "var(--bad)" : (props.b.level ?? 0) <= 30 ? "var(--warn)" : "var(--good)");
  return (
    <div class="bat">
      <div class="bat-ring">
        <svg viewBox="0 0 36 36" aria-hidden="true">
          <circle cx="18" cy="18" r="15.9" class="trk" />
          <circle cx="18" cy="18" r="15.9" class="val" style={{ stroke: color(), "stroke-dasharray": `${props.b.level ?? 0} 100` }} />
        </svg>
        <span class="bat-ic">{props.icon}</span>
      </div>
      <div class="bat-txt">
        <span class="num big">{props.b.level === null ? "—" : `${props.b.level}%`}</span>
        <span class="hint">
          {props.label}
          {props.b.charging ? ` · ${ft("bat.charging")} ⚡` : ""}
        </span>
      </div>
    </div>
  );
}

const SCENES: NoiseScene[] = ["adaptive", "indoor", "commuting", "noisy", "anti_wind"];

/** The result of the last write: confirmed (with "Desfazer") or failed
 *  (with "Tentar de novo" and the Diagnóstico link) */
function WriteBanner(props: { s: QcyState; busy: boolean; send: (c: QcyChange) => Promise<boolean>; diag: () => void }) {
  const offline = () => props.s.link.state !== "connected";
  return (
    <Show when={props.s.write}>
      {(w) => (
        <Show
          when={w().status === "confirmed"}
          fallback={
            <Banner
              kind="error"
              title={ft("write.fail_title", settingName(w().setting))}
              actions={
                <>
                  <Show when={writeChange(w(), "retry")}>
                    {(c) => (
                      <Button kind="strong" busy={props.busy} disabled={props.busy || offline()} onClick={() => void props.send(c())}>
                        {ft("write.retry")}
                      </Button>
                    )}
                  </Show>
                  <Button kind="subtle" onClick={props.diag}>
                    {ft("write.diag")}
                  </Button>
                </>
              }
            >
              {failureText(w().failure)}
            </Banner>
          }
        >
          <Banner
            kind="good"
            title={writeTitle(w())}
            actions={
              <Show when={writeChange(w(), "undo")}>
                {(c) => (
                  <Button busy={props.busy} disabled={props.busy || offline()} onClick={() => void props.send(c())}>
                    {ft("write.undo", valueText(w(), w().previous))}
                  </Button>
                )}
              </Show>
            }
          >
            <Show when={w().previous !== null}>{ft("write.previous", valueText(w(), w().previous))}</Show>
          </Banner>
        </Show>
      )}
    </Show>
  );
}

/** The writable settings of this version (phase F2) */
export const F2_SETTINGS = ["game_mode", "sleep_mode", "wind_reduction"] as const;

/** The name a setting has on screen */
export function settingName(k: string): string {
  switch (k) {
    case "game_mode":
      return ft("game");
    case "sleep_mode":
      return ft("sleep");
    case "wind_reduction":
      return ft("wind");
    case "prompt_volume":
      return ft("prompt_volume");
    default:
      return k;
  }
}

const isLevel = (w: QcyWrite) => w.setting === "prompt_volume";
/** What the write asked for: a boolean, or the prompt volume level */
const target = (w: QcyWrite): boolean | number | null => (isLevel(w) ? (w.value ?? null) : w.enabled);

/** A value of this write's setting as text ("ligado", "4/15") */
export function valueText(w: QcyWrite, v: boolean | number | null | undefined): string {
  if (v === null || v === undefined) return "?";
  if (typeof v === "number") return w.max ? `${v}/${w.max}` : String(v);
  return ft(v ? "write.on" : "write.off");
}

function writeTitle(w: QcyWrite): string {
  if (isLevel(w)) return ft("write.ok_level", settingName(w.setting), valueText(w, w.value));
  return ft(w.enabled ? "write.ok_on" : "write.ok_off", settingName(w.setting));
}

/** The one change "Tentar de novo" (same value) or "Desfazer" (back to the
 *  value read before) sends; null when there is none. Only F2 settings. */
export function writeChange(w: QcyWrite, which: "retry" | "undo"): QcyChange | null {
  const v = which === "retry" ? target(w) : w.previous;
  if (v === null || v === undefined) return null;
  if (which === "undo" && v === target(w)) return null;
  if (isLevel(w)) return typeof v === "number" ? { kind: "prompt_volume", value: v } : null;
  if (typeof v !== "boolean") return null;
  if (w.setting === "game_mode" || w.setting === "sleep_mode" || w.setting === "wind_reduction") return { kind: w.setting, enabled: v };
  return null;
}

/** Wind reduction only works with noise cancelling on; an unread mode
 *  (null) does not grey it, the read-back confirms the write */
export function windAllowed(s: QcyState): boolean {
  return s.noise_mode === "anc" || s.noise_mode === null;
}

/** The prompt volume's maximum: what the earbuds reported, else 15 */
export function promptMax(s: QcyState): number {
  return s.prompt_volume_max > 0 ? s.prompt_volume_max : 15;
}

function failureText(f: QcyWrite["failure"]) {
  switch (f) {
    case "timeout":
    case "mismatch":
    case "no_previous":
    case "send_failed":
    case "out_of_range":
      return ft(`write.fail.${f}`);
    default:
      return ft("write.fail.link");
  }
}

/** The steps of the last write, under its control */
export function writeStepLines(w: QcyWrite): string[] {
  const line = (st: QcyWrite["steps"][number]): string | null => {
    switch (st.what) {
      case "read_previous":
        return w.previous === null ? null : ft("write.step.read_previous", valueText(w, w.previous));
      case "read_previous_failed":
        return ft("write.step.no_previous");
      case "sent":
        return isLevel(w)
          ? ft("write.step.sent_level", settingName(w.setting), valueText(w, w.value))
          : ft("write.step.sent", ft(w.enabled ? "write.turn_on" : "write.turn_off"), settingName(w.setting));
      case "send_failed":
        return ft("write.step.send_failed");
      case "read_back":
        return w.status === "confirmed" ? ft("write.step.read_back", valueText(w, target(w))) : ft("write.step.read_back_bad", st.detail);
      case "no_read_back":
        return ft("write.step.no_read_back");
      case "refused":
        return ft("write.step.refused");
      default:
        return null;
    }
  };
  return w.steps.map(line).filter((l): l is string => l !== null);
}

function WriteSteps(props: { w: QcyWrite }) {
  return (
    <ol class="write-steps hint">
      <For each={writeStepLines(props.w)}>{(l) => <li>{l}</li>}</For>
    </ol>
  );
}

function MainTab(props: {
  s: QcyState;
  send: (c: QcyChange) => Promise<boolean>;
  can?: (kind: string) => boolean;
  partial?: boolean;
  busy?: boolean;
  /** Writable settings already confirmed on the earbuds (tag Verificado) */
  verified?: string[];
}) {
  const s = () => props.s;
  const can = (k: string) => props.can?.(k) ?? true;
  const ok = (k: string) => props.verified?.includes(k) === true;
  const locked = (k: string) => props.partial === true && !can(k);
  /** The tag of a control in the partial (write test) mode */
  const tag = (k: string) =>
    props.partial ? (
      <span
        class="codec-badge fones-tag"
        classList={{ "tag-verified": can(k) && ok(k), "tag-testing": can(k) && !ok(k), "tag-unverified": !can(k) }}
      >
        {ft(!can(k) ? "tag.unverified" : ok(k) ? "tag.verified" : "tag.testing")}
      </span>
    ) : undefined;
  const writeFor = (k: string) => (props.partial && s().write?.setting === k ? s().write! : null);
  const off = (k: string) => props.partial === true && (props.busy === true || !can(k));
  return (
    <>
      <section class="card dev">
        <div class="dev-l">
          <div class="dev-name">{s().device_name}</div>
          <div class="dev-sub">
            <span class="fones-dot" classList={{ good: s().link.state === "connected", warn: s().link.state !== "connected" }} /> {linkText(s())}
          </div>
          <div class="inline">
            <CodecBadge />
            <Show when={s().firmware}>{(f) => <span class="codec-badge">{ft("firmware", f())}</span>}</Show>
          </div>
        </div>
        <div class="bats">
          <BatteryRing b={s().battery.left} label={ft("bat.left")} icon={<BudIcon />} />
          <BatteryRing b={s().battery.case} label={ft("bat.case")} icon={<CaseIcon />} />
          <BatteryRing b={s().battery.right} label={ft("bat.right")} icon={<BudIcon right />} />
        </div>
      </section>

      <Section
        title={ft("noise.title")}
        actions={
          <span class="hint">
            {tag("noise")} {live(s()) ? ft("noise.synced") : ft("last_value")}
          </span>
        }
      >
        <div class="fones-lock" classList={{ locked: locked("noise") }} inert={locked("noise") ? true : undefined}>
        <Segmented<NoiseMode>
          label={ft("noise.title")}
          value={s().noise_mode}
          options={[
            { value: "off", label: ft("noise.off") },
            { value: "anc", label: ft("noise.anc") },
            { value: "transparency", label: ft("noise.transparency") },
          ]}
          onChange={(mode) => void props.send({ kind: "noise", mode })}
        />
        <span class="hint">{ft("noise.scene_hint")}</span>
        <div class="scenes" role="radiogroup" aria-label={ft("noise.scene_hint")}>
          <For each={SCENES}>
            {(sc) => (
              <button
                type="button"
                role="radio"
                class="scene"
                aria-checked={s().noise_scene === sc}
                classList={{ on: s().noise_scene === sc }}
                disabled={s().noise_mode !== "anc"}
                onClick={() => void props.send({ kind: "noise", mode: "anc", scene: sc })}
              >
                <b>{ft(`scene.${sc}`)}</b>
                <span class="hint">{ft(`scene.${sc}.hint`)}</span>
              </button>
            )}
          </For>
        </div>
        </div>
      </Section>

      <Section title={ft("modes.title")} actions={<LastValue s={s()} />}>
        <Toggle
          label={ft("game")}
          badge={tag("game_mode")}
          hint={props.busy && props.partial ? ft("write.sending") : ft("game.hint")}
          checked={s().game_mode}
          disabled={off("game_mode")}
          onChange={(enabled) => void props.send({ kind: "game_mode", enabled })}
        />
        <Show when={writeFor("game_mode")}>{(w) => <WriteSteps w={w()} />}</Show>
        <div class="fones-lock" classList={{ locked: locked("sleep_mode") }} inert={locked("sleep_mode") ? true : undefined}>
          <Toggle
            label={ft("sleep")}
            badge={tag("sleep_mode")}
            hint={props.busy && props.partial ? ft("write.sending") : props.partial ? ft("sleep.warn") : ft("sleep.hint")}
            checked={s().sleep_mode}
            disabled={off("sleep_mode")}
            onChange={(enabled) => void props.send({ kind: "sleep_mode", enabled })}
          />
        </div>
        <Show when={writeFor("sleep_mode")}>{(w) => <WriteSteps w={w()} />}</Show>
        <div
          class="fones-lock"
          classList={{ locked: locked("wind_reduction"), "needs-anc": !windAllowed(s()) }}
          inert={locked("wind_reduction") ? true : undefined}
        >
          <Toggle
            label={ft("wind")}
            badge={tag("wind_reduction")}
            hint={!windAllowed(s()) ? ft("wind.needs_anc") : props.busy && props.partial ? ft("write.sending") : ft("wind.hint")}
            checked={s().wind_reduction}
            disabled={off("wind_reduction") || !windAllowed(s())}
            onChange={(enabled) => {
              if (windAllowed(s())) void props.send({ kind: "wind_reduction", enabled });
            }}
          />
        </div>
        <Show when={writeFor("wind_reduction")}>{(w) => <WriteSteps w={w()} />}</Show>
      </Section>
    </>
  );
}

const live = (s: QcyState) => s.link.state === "connected";

/** The small curve of a preset chip */
/** Atualizar: always usable unless busy or already connecting. It is the
 *  only way to open the headset link after a core restart (link "none"),
 *  in read-only and writable builds alike; refresh only reads. */
export function canRefresh(s: QcyState | null | undefined, busy: boolean): boolean {
  return !busy && s?.link.state !== "connecting";
}

export function curvePath(gains: number[], w = 56, h = 18): string {
  const y = (g: number) => h / 2 - (g / EQ_GAIN_MAX) * (h / 2 - 1);
  return gains.map((g, i) => `${i ? "L" : "M"}${((i * w) / (gains.length - 1)).toFixed(1)} ${y(g).toFixed(1)}`).join(" ");
}

function freqLabel(hz: number) {
  return hz >= 1000 ? `${hz / 1000}k` : String(hz);
}

function EqTab(props: { s: QcyState; send: (c: QcyChange) => Promise<boolean>; notify: (t: string) => void }) {
  const [draft, setDraft] = createSignal<number[]>([...props.s.eq.gains]);
  const [master, setMaster] = createSignal(props.s.eq.master_gain);
  // A new state from the headset replaces the draft
  createEffect(() => {
    setDraft([...props.s.eq.gains]);
    setMaster(props.s.eq.master_gain);
  });
  const preset = () => matchPreset(draft(), master());
  const dirty = () => draft().some((g, i) => g !== props.s.eq.gains[i]) || master() !== props.s.eq.master_gain;
  const apply = async () => {
    if (await props.send({ kind: "eq", eq: { preset: preset(), gains: draft(), master_gain: master() } })) props.notify(ft("applied"));
  };
  const name = (id: string) => ft(`eq.preset.${id}` as Parameters<typeof ft>[0]);

  return (
    <>
      <Section
        title={ft("eq.title")}
        actions={
          <>
            <LastValue s={props.s} />
            <Show when={master() !== 0}>
              <span class="hint">{ft("eq.master", master())}</span>
            </Show>
            <Button kind="subtle" icon="arrow_reset" onClick={() => (setDraft(draft().map(() => 0)), setMaster(0))}>
              {ft("eq.reset")}
            </Button>
            <Button kind="strong" disabled={!dirty()} onClick={() => void apply()}>
              {ft("eq.apply")}
            </Button>
          </>
        }
      >
        <div class="chips" role="radiogroup" aria-label={ft("eq.title")}>
          <For each={EQ_PRESETS}>
            {(p) => (
              <button
                type="button"
                role="radio"
                class="chip"
                aria-checked={preset() === p.id}
                classList={{ on: preset() === p.id }}
                onClick={() => (setDraft([...p.gains]), setMaster(p.master_gain))}
              >
                <svg width="56" height="18" viewBox="0 0 56 18" aria-hidden="true">
                  <path d={curvePath(p.gains)} fill="none" stroke="currentColor" stroke-width="1.4" />
                </svg>
                <span>{name(p.id)}</span>
              </button>
            )}
          </For>
          <span class="chip custom" classList={{ on: preset() === "custom" }} aria-hidden={preset() !== "custom"}>
            <svg width="56" height="18" viewBox="0 0 56 18" aria-hidden="true">
              <path d={curvePath(draft())} fill="none" stroke="currentColor" stroke-width="1.4" stroke-dasharray="3 2" />
            </svg>
            <span>{ft("eq.custom")}</span>
          </span>
        </div>
        <div class="eq">
          <For each={[...EQ_BANDS_HZ]}>
            {(hz, i) => (
              <label class="band">
                <span class="gain num">{(draft()[i()] > 0 ? "+" : "") + draft()[i()].toFixed(1)}</span>
                <input
                  type="range"
                  class="vslider"
                  min={EQ_GAIN_MIN}
                  max={EQ_GAIN_MAX}
                  step={0.5}
                  value={draft()[i()]}
                  aria-label={`${freqLabel(hz)} Hz`}
                  onInput={(e) => {
                    const v = Number(e.currentTarget.value);
                    setDraft(draft().map((g, j) => (j === i() ? v : g)));
                  }}
                />
                <span class="freq">{freqLabel(hz)}</span>
              </label>
            )}
          </For>
        </div>
      </Section>

      <GesturesSection s={props.s} send={props.send} />
    </>
  );
}

const GESTURE_ROWS: GestureKind[] = ["single_tap", "double_tap", "triple_tap", "long_press"];

function GesturesSection(props: { s: QcyState; send: (c: QcyChange) => Promise<boolean> }) {
  const options = (): Option<string>[] => TOUCH_ACTIONS.map((a) => ({ value: a, label: ft(`action.${a}`) }));
  return (
    <Section title={ft("gestures.title")} actions={<LastValue s={props.s} />}>
      <table class="gestures">
        <thead>
          <tr>
            <th>{ft("gestures.gesture")}</th>
            <th>{ft("gestures.left")}</th>
            <th>{ft("gestures.right")}</th>
          </tr>
        </thead>
        <tbody>
          <For each={GESTURE_ROWS}>
            {(g) => {
              const ok = () => props.s.supported_gestures.includes(g);
              const cell = (side: Side) => (
                <td>
                  <Select<string>
                    label={`${ft(`gestures.${g}`)} · ${ft(`gestures.${side}`)}`}
                    value={ok() ? (props.s.gestures[side][g] ?? "none") : "none"}
                    disabled={!ok()}
                    options={options()}
                    onChange={(action) => void props.send({ kind: "gesture", side, gesture: g, action: action as (typeof TOUCH_ACTIONS)[number] })}
                  />
                </td>
              );
              return (
                <tr classList={{ "dis-row": !ok() }}>
                  <td>
                    {ft(`gestures.${g}`)}
                    <Show when={!ok()}>
                      <span class="tag">{ft("gestures.unsupported")}</span>
                    </Show>
                  </td>
                  {cell("left")}
                  {cell("right")}
                </tr>
              );
            }}
          </For>
        </tbody>
      </table>
    </Section>
  );
}

const AUTO_OFF: AutoOff[] = [null, 5, 15, 30, 60];

function SettingsTab(props: {
  s: QcyState;
  send: (c: QcyChange) => Promise<boolean>;
  onLdacOff: () => void;
  save: () => void;
  can?: (kind: string) => boolean;
  partial?: boolean;
  busy?: boolean;
  verified?: string[];
}) {
  const s = () => props.s;
  const can = (k: string) => props.can?.(k) ?? true;
  /** In the partial (write test) mode this tab stays greyed and inert
   *  (the prompt volume is shown, read-only) */
  const locked = (k: string) => props.partial === true && !can(k);
  const lockAll = () => props.partial === true;
  // The switch stays on until the dialog's answer: the click flipped the
  // native checkbox, so it is put back (off -> on makes Solid set it again)
  const [ldacUi, setLdacUi] = createSignal(true);
  const savedAt = () => {
    const t = s().profile_saved_at;
    return t === null ? ft("profile.none") : new Date(t).toLocaleString(lang(), { dateStyle: "short", timeStyle: "short" });
  };
  return (
    <>
      <Section title={ft("use.title")} actions={<LastValue s={s()} />}>
        <div class="fones-lock" classList={{ locked: lockAll() }} inert={lockAll() ? true : undefined}>
        <Toggle label={ft("wear")} hint={ft("wear.hint")} checked={s().wear_detection} onChange={(enabled) => void props.send({ kind: "wear_detection", enabled })} />
        <Toggle
          label={ft("ldac")}
          badge={<span class="tag warn">{ft("ldac.tag")}</span>}
          hint={ft("ldac.hint")}
          checked={s().ldac && ldacUi()}
          onChange={(enabled) => {
            if (enabled) void props.send({ kind: "ldac", enabled });
            else {
              setLdacUi(false);
              queueMicrotask(() => setLdacUi(true));
              props.onLdacOff();
            }
          }}
        />
        <Toggle label={ft("multipoint")} hint={ft("multipoint.hint")} checked={s().multipoint} onChange={(enabled) => void props.send({ kind: "multipoint", enabled })} />
        </div>
        {/* Read-only: the N70 did not take 0x1D writes (fase 2b) */}
        <div class="row">
          <div class="row-text">
            <span class="row-label">{ft("prompt_volume")}</span>
            <span class="hint">{ft("prompt_volume.readonly")}</span>
          </div>
          <span class="num">
            {Math.max(0, Math.min(promptMax(s()), s().prompt_volume))}/{promptMax(s())}
          </span>
        </div>
        <div class="fones-lock" classList={{ locked: lockAll() }} inert={lockAll() ? true : undefined}>
        <div class="row">
          <div class="row-text">
            <span class="row-label">{ft("auto_off")}</span>
            <span class="hint">{ft("auto_off.hint")}</span>
          </div>
          <Select<string>
            label={ft("auto_off")}
            value={String(s().auto_off)}
            options={AUTO_OFF.map((m) => ({ value: String(m), label: m === null ? ft("auto_off.never") : ft("auto_off.min", m) }))}
            onChange={(v) => void props.send({ kind: "auto_off", minutes: (v === "null" ? null : Number(v)) as AutoOff })}
          />
        </div>
        </div>
      </Section>

      <Section title={ft("profile.title")}>
        <div class="fones-lock" classList={{ locked: locked("reapply_on_connect") }} inert={locked("reapply_on_connect") ? true : undefined}>
        <Toggle label={ft("reapply")} hint={ft("reapply.hint")} checked={s().reapply_on_connect} onChange={(enabled) => void props.send({ kind: "reapply_on_connect", enabled })} />
        <div class="row">
          <div class="row-text">
            <span class="row-label">{ft("profile.saved")}</span>
            <span class="hint">{savedAt()}</span>
          </div>
          <Button onClick={() => props.save()}>{ft("profile.save")}</Button>
        </div>
        </div>
      </Section>
    </>
  );
}

