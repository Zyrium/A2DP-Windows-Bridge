// The Auracast matrix mixer (Developer screen, under the broadcast form):
// which source speakers (columns FL FR FC LFE BL BR SL SR) make each output
// (rows FL FR RL RR), plus a delay and polarity per output. Automatic from
// three levels, or each gain by hand; named presets. Every change is saved
// (auracast.mix_set) and, while a broadcast runs, heard at once.

import { createEffect, createSignal, For, type JSX, on, onCleanup, Show } from "solid-js";
import { useApp } from "../app/context";
import { Button, Check, Section, Segmented, Select } from "../components/controls";
import { t } from "../i18n";
import type { AuracastMix, AuracastMixState } from "../ipc/types";

export const INPUT_NAMES = ["FL", "FR", "FC", "LFE", "BL", "BR", "SL", "SR"] as const;
export const OUTPUT_NAMES = ["FL", "FR", "RL", "RR"] as const;
/** Sound in air at about 20 °C: metres per millisecond */
export const M_PER_MS = 0.343;
const MAX_DELAY_MS = 500;
const MAX_GAIN = 4;

type Unit = "ms" | "m";

/** A gain as the grid shows it */
export const gainText = (g: number) => (g === 0 ? "0" : g.toFixed(3).replace(/\.?0+$/, ""));

/** A delay field's value in `unit` (ms kept to 0.01) */
export function delayIn(ms: number, unit: Unit): string {
  return unit === "ms" ? String(Math.round(ms * 100) / 100) : String(Math.round(ms * M_PER_MS * 10000) / 10000);
}

/** A delay typed in `unit`, in ms within range (null: not a number) */
export function delayFrom(text: string, unit: Unit): number | null {
  const v = Number(text.replace(",", "."));
  if (text.trim() === "" || !Number.isFinite(v)) return null;
  const ms = unit === "ms" ? v : v / M_PER_MS;
  return Math.min(MAX_DELAY_MS, Math.max(0, Math.round(ms * 100) / 100));
}

const num = (text: string, lo: number, hi: number): number | null => {
  const v = Number(text.replace(",", "."));
  if (text.trim() === "" || !Number.isFinite(v)) return null;
  return Math.min(hi, Math.max(lo, v));
};

const presetLabel = (name: string, builtin: boolean) => (builtin ? t(`ui.dev.auracast_mix_preset_${name}`) : name);

/** `children`: the broadcast's own settings at the top of the frame */
export function AuracastMixer(props: { rear: boolean; live: boolean; children?: JSX.Element }) {
  const app = useApp();
  const { core } = app;
  const [state, setState] = createSignal<AuracastMixState | null>(null);
  const [unit, setUnit] = createSignal<Unit>("ms");
  const [preset, setPreset] = createSignal("");
  const [presetName, setPresetName] = createSignal("");

  const load = () =>
    core
      .request("auracast.mix", { channels: null, rear: props.rear })
      .then(setState)
      .catch(() => {});
  createEffect(on([() => props.rear, () => props.live], () => void load()));

  // Changes go out after a short pause (typing, dragging); an answer is
  // taken only when nothing changed since it was asked
  let timer: ReturnType<typeof setTimeout> | undefined;
  let edits = 0;
  onCleanup(() => clearTimeout(timer));
  const change = (patch: Partial<AuracastMix>) => {
    const st = state();
    if (!st) return;
    const mix = { ...st.mix, ...patch };
    setState({ ...st, mix });
    const n = ++edits;
    clearTimeout(timer);
    timer = setTimeout(() => {
      void app.run(async () => {
        const r = await core.request("auracast.mix_set", { mix, channels: null, rear: props.rear });
        if (n === edits) setState(r);
      });
    }, 120);
  };

  const mix = () => state()!.mix;
  const cell = (row: number, col: number) => (mix().auto ? state()!.effective : mix().matrix)[row]?.[col] ?? 0;
  const setCell = (row: number, col: number, g: number) => {
    const m = mix().matrix.map((r) => [...r]);
    m[row]![col] = g;
    change({ matrix: m });
  };
  /** By hand, starting from the matrix that plays */
  const manual = () => change({ auto: false, matrix: state()!.effective.map((r) => [...r]) });
  const setAt = <T,>(list: T[], i: number, v: T) => list.map((x, j) => (j === i ? v : x));

  const choosePreset = (name: string) => {
    setPreset(name);
    const p = state()?.presets.find((x) => x.name === name);
    if (!p) return;
    if (!p.builtin) setPresetName(p.name);
    change({ ...p.mix });
  };
  const savePreset = async () => {
    const name = presetName().trim();
    await app.run(async () => {
      const r = await core.request("auracast.mix_preset_save", { name, mix: mix() });
      setState({ ...r, mix: mix() });
      setPreset(name);
    });
  };
  const selected = () => state()?.presets.find((p) => p.name === preset());
  /** A name, and not a built-in preset's (the core refuses those) */
  const canSave = () => {
    const name = presetName().trim();
    return name !== "" && !state()?.presets.some((p) => p.builtin && p.name === name);
  };
  const deletePreset = async () => {
    const name = preset();
    await app.run(async () => {
      const r = await core.request("auracast.mix_preset_delete", { name });
      setState({ ...r, mix: mix() });
      setPreset("");
    });
  };

  return (
    <Show when={state()}>
      {(st) => (
        <Section title={t("ui.dev.auracast_mix")}>
          {props.children}
          <p class="hint">{st().live ? t("ui.dev.auracast_mix_live") : t("ui.dev.auracast_mix_next")}</p>
          <div class="row">
            <span class="row-text">
              <span class="row-label">{t("ui.dev.auracast_mix_preset")}</span>
            </span>
            <Select<string>
              label={t("ui.dev.auracast_mix_preset")}
              value={preset()}
              options={[
                { value: "", label: t("ui.dev.auracast_mix_preset_pick"), disabled: true },
                ...st().presets.map((p) => ({
                  value: p.name,
                  label: presetLabel(p.name, p.builtin),
                  group: p.builtin ? t("ui.dev.auracast_mix_builtin") : t("ui.dev.auracast_mix_mine"),
                })),
              ]}
              onChange={choosePreset}
            />
          </div>
          <div class="row mix-preset-save">
            <input
              class="input"
              type="text"
              value={presetName()}
              placeholder={t("ui.dev.auracast_mix_preset_name")}
              aria-label={t("ui.dev.auracast_mix_preset_name")}
              onInput={(e) => setPresetName(e.currentTarget.value)}
            />
            <Button icon="checkmark" disabled={!canSave()} onClick={savePreset}>
              {t("ui.dev.auracast_mix_save")}
            </Button>
            <Show when={selected() && !selected()!.builtin}>
              <Button kind="danger" icon="delete" onClick={deletePreset}>
                {t("ui.dev.auracast_mix_delete")}
              </Button>
            </Show>
          </div>

          <div class="row">
            <span class="row-text">
              <span class="row-label">{t("ui.dev.auracast_mix_auto")}</span>
              <span class="hint">{t("ui.dev.auracast_mix_auto_hint")}</span>
            </span>
            <input
              type="checkbox"
              role="switch"
              class="switch"
              checked={mix().auto}
              aria-label={t("ui.dev.auracast_mix_auto")}
              onChange={(e) => (e.currentTarget.checked ? change({ auto: true }) : manual())}
            />
          </div>
          <Show when={mix().auto}>
            <div class="mix-levels">
              <Level label={t("ui.dev.auracast_mix_center")} value={mix().center_db} onChange={(v) => change({ center_db: v })} />
              <Level label={t("ui.dev.auracast_mix_lfe")} value={mix().lfe_db} onChange={(v) => change({ lfe_db: v })} />
              <Level
                label={t("ui.dev.auracast_mix_surround")}
                value={mix().surround_db}
                onChange={(v) => change({ surround_db: v })}
              />
              <Check label={t("ui.dev.auracast_mix_normalize")} checked={mix().normalize} onChange={(v) => change({ normalize: v })} />
            </div>
          </Show>

          <div class="mix-scroll">
            <table class="mix-grid">
              <thead>
                <tr>
                  <th scope="col">{t("ui.dev.auracast_mix_output")}</th>
                  <For each={INPUT_NAMES}>
                    {(name, c) => (
                      <th scope="col" classList={{ "mix-absent": !st().present[c()] }}>
                        {name}
                      </th>
                    )}
                  </For>
                  <th scope="col">
                    {t("ui.dev.auracast_mix_delay")} ({unit() === "ms" ? t("ui.dev.auracast_mix_ms") : t("ui.dev.auracast_mix_m")})
                  </th>
                  <th scope="col">{t("ui.dev.auracast_mix_invert")}</th>
                </tr>
              </thead>
              <tbody>
                <For each={OUTPUT_NAMES}>
                  {(out, r) => (
                    <tr classList={{ "mix-absent": r() >= 2 && !st().rear }}>
                      <th scope="row">{out}</th>
                      <For each={INPUT_NAMES}>
                        {(inp, c) => (
                          <td classList={{ "mix-absent": !st().present[c()], "mix-on": cell(r(), c()) !== 0 }}>
                            <input
                              class="input mix-cell"
                              type="number"
                              step="0.05"
                              min={-MAX_GAIN}
                              max={MAX_GAIN}
                              readOnly={mix().auto}
                              value={gainText(cell(r(), c()))}
                              aria-label={`${inp} → ${out}`}
                              onDblClick={() => mix().auto && manual()}
                              onChange={(e) => {
                                const g = num(e.currentTarget.value, -MAX_GAIN, MAX_GAIN);
                                if (g !== null) setCell(r(), c(), g);
                                else e.currentTarget.value = gainText(cell(r(), c()));
                              }}
                            />
                          </td>
                        )}
                      </For>
                      <td>
                        <input
                          class="input mix-delay"
                          type="number"
                          step={unit() === "ms" ? "0.01" : "0.001"}
                          min="0"
                          value={delayIn(mix().delay_ms[r()] ?? 0, unit())}
                          aria-label={`${t("ui.dev.auracast_mix_delay")} ${out}`}
                          onChange={(e) => {
                            const ms = delayFrom(e.currentTarget.value, unit());
                            if (ms !== null) change({ delay_ms: setAt(mix().delay_ms, r(), ms) });
                            else e.currentTarget.value = delayIn(mix().delay_ms[r()] ?? 0, unit());
                          }}
                        />
                      </td>
                      <td>
                        <input
                          type="checkbox"
                          role="switch"
                          class="switch small"
                          checked={mix().invert[r()] ?? false}
                          aria-label={`${t("ui.dev.auracast_mix_invert")} ${out}`}
                          onChange={(e) => change({ invert: setAt(mix().invert, r(), e.currentTarget.checked) })}
                        />
                      </td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
          <Show when={!st().rear}>
            <p class="hint">{t("ui.dev.auracast_mix_no_rear")}</p>
          </Show>
          <div class="row">
            <span class="row-text">
              <span class="row-label">{t("ui.dev.auracast_mix_unit")}</span>
              <span class="hint">{t("ui.dev.auracast_mix_delay_hint")}</span>
            </span>
            <Segmented<Unit>
              label={t("ui.dev.auracast_mix_unit")}
              value={unit()}
              options={[
                { value: "ms", label: t("ui.dev.auracast_mix_ms") },
                { value: "m", label: t("ui.dev.auracast_mix_m") },
              ]}
              onChange={setUnit}
            />
          </div>
        </Section>
      )}
    </Show>
  );
}

function Level(props: { label: string; value: number; onChange: (v: number) => void }) {
  return (
    <label class="mix-level">
      <span>{props.label}</span>
      <input
        class="input"
        type="number"
        step="0.5"
        min="-60"
        max="12"
        value={props.value}
        onChange={(e) => {
          const v = num(e.currentTarget.value, -60, 12);
          if (v !== null) props.onChange(v);
          else e.currentTarget.value = String(props.value);
        }}
      />
    </label>
  );
}
