// The Fones screen: control of the QCY headset over BLE (phase 4, UI
// only). Talks to a QcyProvider (ipc/qcy.ts); the audio stream is the
// core's and is only shown here (codec badge).

import { createEffect, createResource, createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import { useApp, errorText } from "../../app/context";
import { Banner, Button, Dialog, Section, Segmented, Select, Toggle, type Option } from "../../components/controls";
import { Icon } from "../../components/Icon";
import { CodecBadge } from "../../components/stream";
import { createQcy, EQ_BANDS_HZ, EQ_GAIN_MAX, EQ_GAIN_MIN, TOUCH_ACTIONS, type AutoOff, type Battery, type GestureKind, type NoiseMode, type NoiseScene, type QcyChange, type QcyProvider, type QcyState, type Side } from "../../ipc/qcy";
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
        <Button icon="arrow_sync" disabled={!live() || busy()} onClick={() => void act((q) => q.refresh())}>
          {ft("refresh")}
        </Button>
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
                {(s().link as { reason?: string }).reason === "not_implemented" ? ft("unavailable.not_implemented") : ft("unavailable.text")}
              </Banner>
            </Show>
            <div class="fones-body" classList={{ stale: !live() }} aria-disabled={!live()} inert={!live() ? true : undefined}>
              <Show when={tab() === "main"}>
                <MainTab s={s()} send={send} />
              </Show>
              <Show when={tab() === "eq"}>
                <EqTab s={s()} send={send} notify={notify} />
              </Show>
              <Show when={tab() === "settings"}>
                <SettingsTab s={s()} send={send} onLdacOff={() => setAskLdac(true)} save={() => void act((q) => q.saveProfile())} />
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

function MainTab(props: { s: QcyState; send: (c: QcyChange) => Promise<boolean> }) {
  const s = () => props.s;
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

      <Section title={ft("noise.title")} actions={<span class="hint">{live(s()) ? ft("noise.synced") : ft("last_value")}</span>}>
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
      </Section>

      <Section title={ft("modes.title")} actions={<LastValue s={s()} />}>
        <Toggle label={ft("game")} hint={ft("game.hint")} checked={s().game_mode} onChange={(enabled) => void props.send({ kind: "game_mode", enabled })} />
        <Toggle label={ft("sleep")} hint={ft("sleep.hint")} checked={s().sleep_mode} onChange={(enabled) => void props.send({ kind: "sleep_mode", enabled })} />
        <Toggle label={ft("wind")} hint={ft("wind.hint")} checked={s().wind_reduction} onChange={(enabled) => void props.send({ kind: "wind_reduction", enabled })} />
      </Section>
    </>
  );
}

const live = (s: QcyState) => s.link.state === "connected";

/** The small curve of a preset chip */
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

function SettingsTab(props: { s: QcyState; send: (c: QcyChange) => Promise<boolean>; onLdacOff: () => void; save: () => void }) {
  const s = () => props.s;
  const [vol, setVol] = createSignal(s().prompt_volume);
  // The switch stays on until the dialog's answer: the click flipped the
  // native checkbox, so it is put back (off -> on makes Solid set it again)
  const [ldacUi, setLdacUi] = createSignal(true);
  createEffect(() => setVol(s().prompt_volume));
  const savedAt = () => {
    const t = s().profile_saved_at;
    return t === null ? ft("profile.none") : new Date(t).toLocaleString(lang(), { dateStyle: "short", timeStyle: "short" });
  };
  return (
    <>
      <Section title={ft("use.title")} actions={<LastValue s={s()} />}>
        <Toggle label={ft("wear")} hint={ft("wear.hint")} checked={s().wear_detection} onChange={(enabled) => void props.send({ kind: "wear_detection", enabled })} />
        <div class="ldac-row">
          <Toggle
            label={ft("ldac")}
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
          <span class="tag warn ldac-tag">{ft("ldac.tag")}</span>
        </div>
        <Toggle label={ft("multipoint")} hint={ft("multipoint.hint")} checked={s().multipoint} onChange={(enabled) => void props.send({ kind: "multipoint", enabled })} />
        <div class="row">
          <div class="row-text">
            <span class="row-label">{ft("prompt_volume")}</span>
          </div>
          <div class="range">
            <input
              type="range"
              min={0}
              max={s().prompt_volume_max}
              step={1}
              value={vol()}
              aria-label={ft("prompt_volume")}
              onInput={(e) => setVol(Number(e.currentTarget.value))}
              onChange={(e) => void props.send({ kind: "prompt_volume", value: Number(e.currentTarget.value) })}
            />
            <span class="num">
              {vol()}/{s().prompt_volume_max}
            </span>
          </div>
        </div>
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
      </Section>

      <Section title={ft("profile.title")}>
        <Toggle label={ft("reapply")} hint={ft("reapply.hint")} checked={s().reapply_on_connect} onChange={(enabled) => void props.send({ kind: "reapply_on_connect", enabled })} />
        <div class="row">
          <div class="row-text">
            <span class="row-label">{ft("profile.saved")}</span>
            <span class="hint">{savedAt()}</span>
          </div>
          <Button onClick={() => props.save()}>{ft("profile.save")}</Button>
        </div>
      </Section>
    </>
  );
}
