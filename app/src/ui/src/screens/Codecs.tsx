// Codecs (issue #86, it replaced Profiles): each saved audio device with
// its codec cards, the codecs it offered when it last connected in green
// and the others in red (hidden unless asked for), each with its settings
// behind "...", and a dot beside the device's name that is green while it
// streams. The cards are dragged into the device's own order (its
// profile's codec_order, issue #86 round 8; with Alt+arrow keys too while
// Auto-select is off), highest sound quality first unless changed; Auto
// takes the first card the device offers. "Auto-select the codec with the
// highest quality" is one setting for every device (settings.auto_codec,
// on by default; each device had its own before): on, the cards show what
// Auto picks, and a click on one, or on its "...", offers to turn
// Auto-select off; off, choosing a card is choosing that device's codec (a
// device with none chosen yet, its profile's codec "auto", goes on with
// Auto's pick). A card the device does not offer is
// chosen like any other (issue #86: without Auto-select the choice is the
// user's): the core asks for it anyway (the profile's codec_forced) and the
// stream stops with an error if the device refuses it. A codec chosen while
// the device offered it and no longer offered (earbuds switched codecs)
// falls back to the first card it offers in that order (lib/codecs.ts
// effectiveCodec, as the core), shown chosen. A codec it may offer
// only once chosen (Device.codecs_on_request) stays a card of its own, chosen
// also while another one streams instead (issue #30). The codec chosen
// stays in the device's profile while Auto-select is on. The device is
// named without its address (shown on Audio Devices).
// Choosing changes the device's profile (made when first needed; the core
// keeps one per device and starts a stream in use again with it). What
// belongs to the device rather than the codec (capture source, volume sync,
// packet size) is on Audio Devices. An LE Audio entry (experimental) has LC3
// only: its cards LC3 (the default) and LC3 LL (the profile's
// le_low_latency, issue #86). aptX Adaptive's low latency mode is a card of
// its own, aptX Adaptive LL (lib/codecs.ts APTXAD_LL: aptX Adaptive with
// quality "ll" in the profile; Auto never takes it). The LE Audio and call
// codec cards' "..." shows the fields of an A2DP codec's, read only.
// The microphone's codec (issue #86): while a classic device's call link
// is open its call codec cards replace the music ones, and while an LE Audio
// device's microphone runs an LC3 card for it follows LC3's. Under
// Auto-select they show what the link took (a call codec card clicked
// offers to turn Auto-select off); off, a call codec card is chosen (the
// profile's voice_codec: the best the link may use, the next lower one the
// headphones list otherwise), and the LE Audio microphone's "..." chooses
// its rate among those the headphones allow (the profile's mic_rate, 0 =
// Auto). The choice stays in the profile while Auto-select is on. The
// microphone stays mono. The call codec cards say which the headphones
// list (CVSD always) while the unsupported ones show. While the call link
// opens again with another codec (A2DP in between) or the LE Audio stream
// is set up again for another rate, the microphone's cards stay, waiting.
// A codec's "..." applies each change at once (issue #86): the codec in use
// is saved, which starts the stream again but for the bitpools, which the
// encoder takes as the slider moves; another codec's settings are only kept
// for when it is chosen.
// A codec chosen while a stream runs starts it again: the cards (and Home's
// Codec card) then wait until it streams again before the next choice
// (issue #86: quick switches left no sound or crackling; a fixed 3 s was
// slow on a quick device and too short on a slow one). With the unsupported codecs hidden, the cards shown need no
// "Supported" tag (issue #86).

import { batch, createEffect, createSignal, createUniqueId, For, on, onCleanup, Show, type JSX } from "solid-js";
import { unwrap } from "solid-js/store";
import { onAudioDevicesChanged, useApp } from "../app/context";
import { saveSettings } from "../app/settings";
import { CodecCard, CodecSettings, type CardSpec, type CodecChoice } from "../components/CodecCards";
import { Banner, Button, Dialog, Field, Select, titleWhenCut, Toggle } from "../components/controls";
import { Icon } from "../components/Icon";
import { stateLabel } from "../components/stream";
import { t } from "../i18n";
import type { AudioDevice, MediaEq, Profile, Snapshot } from "../ipc/types";
import {
  autoKeepsCodecSettings,
  CARD_ORDER,
  cardOf,
  cardOrder,
  CODECS,
  codecKey,
  codecLabelKey,
  codecSpec,
  codecSupport,
  developerOnly,
  effectiveCodec,
  offeredCodecs,
  capsForSettings,
  sbcCapsForSettings,
  isDefaultOrder,
  moved,
  type CodecId,
  type CodecSpec,
  type CodecSupport,
} from "../lib/codecs";
import {
  audioEntries,
  captureDevice,
  codecsOnRequest,
  entryBase,
  entryByKey,
  followedName,
  keptFor,
  knownCodecCaps,
  knownCodecs,
  normalizeProfile,
  withCodec,
  type AudioEntry,
} from "../lib/profile";
import { knownSbcCaps } from "../lib/sbc";
import { entryState } from "./Home";

const HIDE_KEY = "bluecaster.codecs.hide_unsupported";

/** A "..." dialog's changes are saved this long after the last one: only
 *  the end of a quick series (a slider moved) */
export const SAVE_DELAY_MS = 120;

const BITPOOLS: readonly string[] = ["sbc_bitpool", "sbcxq_bitpool", "sbc_min_bitpool", "sbcxq_min_bitpool"];

/** `values` differ from the profile's in the bitpools alone */
function onlyBitpools(p: Profile, values: Omit<CodecChoice, "codec">): boolean {
  return (Object.keys(values) as (keyof typeof values)[]).every((k) => BITPOOLS.includes(k) || p[k] === values[k]);
}

/** After a codec chosen during a stream, the codec cards wait at least
 *  this long, and until the stream is no longer starting again... */
export const SETTLE_MIN_MS = 1000;
/** ...but never longer than this (a start that hangs) */
export const SETTLE_MAX_MS = 15000;
// For every device and across pages: one stream starts again at a time
const [settling, setSettling] = createSignal(false);
export { settling };
let settleTimer: ReturnType<typeof setInterval> | undefined;

/** The stream is being started (again) */
export function restarting(snap: Snapshot | null | undefined): boolean {
  const st = snap?.stream.state;
  return st === "connecting" || st === "reconnecting";
}

/** The codec cards wait for the stream started again with a new codec */
export function settle(app: ReturnType<typeof useApp>) {
  clearInterval(settleTimer);
  const since = Date.now();
  setSettling(true);
  settleTimer = setInterval(() => {
    const waited = Date.now() - since;
    if ((waited >= SETTLE_MIN_MS && !restarting(app.core.state.snap)) || waited >= SETTLE_MAX_MS) {
      clearInterval(settleTimer);
      setSettling(false);
    }
  }, 200);
}

/** The cards no longer wait (a core gone, the tests) */
export function endSettle() {
  clearInterval(settleTimer);
  setSettling(false);
}

/** Resolves once the codec cards no longer wait */
function untilSettled(): Promise<void> {
  return new Promise((done) => {
    const check = () => (settling() ? setTimeout(check, 100) : done());
    check();
  });
}

function loadHide(): boolean {
  try {
    return localStorage.getItem(HIDE_KEY) !== "0";
  } catch {
    return true;
  }
}

/** Saves the device's profile as `edit` changes it (the profile is made
 *  when the device has none yet); its name follows the codec while it is
 *  "<device> | <codec>". The profile's name once saved, None if that failed. */
async function saveEntry(
  app: ReturnType<typeof useApp>,
  entry: AudioEntry,
  edit: (p: Profile) => Profile,
): Promise<string | null> {
  const snap = app.core.state.snap!;
  const cur = entry.profile ? structuredClone(unwrap(entry.profile)) : null;
  const next = edit(cur ?? entryBase(entry, snap.profiles));
  const taken = snap.profiles.map((p) => p.name).filter((n) => n !== cur?.name);
  next.name = followedName(next, entry.le_audio ? "" : t(codecSpec(cardOf(next)).label), taken);
  if (cur && JSON.stringify(next) === JSON.stringify(cur)) return cur.name;
  const saved = normalizeProfile(next);
  const ok = await app.run(() => app.core.request("profile.save", { profile: saved, original_name: cur?.name ?? null }));
  return ok ? saved.name : null;
}

/** Saves a device's headset mode, microphone output, default input, the
 *  microphone's processing (Audio Devices) or mute (Home's and the flyout's
 *  chip). The core opens or closes the microphone link (Auto: as apps record
 *  from the output); the processing applies in place. True once saved. */
export async function saveHeadsetMic(
  app: ReturnType<typeof useApp>,
  entry: AudioEntry,
  patch: Partial<
    Pick<
      Profile,
      "headset_mic" | "mic_device_id" | "mic_device_name" | "mic_muted" | "mic_default_input" | "mic_processing"
    >
  >,
): Promise<boolean> {
  return (await saveEntry(app, entry, (base) => ({ ...base, ...patch }))) !== null;
}

/** Saves the microphone's codec chosen for a device: the call codec of a
 *  classic one, the microphone's rate (Hz, 0 = Auto) of an LE Audio one.
 *  The core takes it in place or sets the stream up again. True once saved. */
export async function saveMicCodec(
  app: ReturnType<typeof useApp>,
  entry: AudioEntry,
  patch: Partial<Pick<Profile, "voice_codec" | "mic_rate">>,
): Promise<boolean> {
  return (await saveEntry(app, entry, (base) => ({ ...base, ...patch }))) !== null;
}

/** Saves a device's playback EQ (Audio Devices). A stream of it takes the
 *  change in place. True once saved. */
export async function saveMediaEq(app: ReturnType<typeof useApp>, entry: AudioEntry, eq: MediaEq): Promise<boolean> {
  return (await saveEntry(app, entry, (base) => ({ ...base, media_eq: eq }))) !== null;
}

/** Saves the device's profile with `codec` chosen (`values`: from its "..."
 *  dialog; `order`: the device's codec order, when one is not saved yet;
 *  `forced`: chosen though the device does not offer it, asked for anyway).
 *  The profile's name once saved, None if that failed. */
export function chooseCodec(
  app: ReturnType<typeof useApp>,
  entry: AudioEntry,
  codec: string,
  values?: Omit<CodecChoice, "codec">,
  order?: readonly CodecId[] | null,
  forced = false,
): Promise<string | null> {
  return saveEntry(app, entry, (base) => {
    if (entry.le_audio) return base;
    const next = { ...withCodec(base, codec, values), codec_forced: forced && codec !== "auto" };
    return order ? { ...next, codec_order: orderKeys(order) } : next;
  });
}

/** Saves the device's profile with `codec` and `quality` (bitrate) chosen. */
export function chooseQuality(
  app: ReturnType<typeof useApp>,
  entry: AudioEntry,
  codec: string,
  quality: string,
): Promise<string | null> {
  return saveEntry(app, entry, (base) => {
    if (entry.le_audio) return base;
    return { ...withCodec(base, codec), quality };
  });
}

/** A codec order as a profile keeps it: none for the default one */
function orderKeys(order: readonly CodecId[]): string[] {
  return isDefaultOrder(order) ? [] : [...order];
}

/** Saves the device's codec order (its cards' order) */
export function saveCodecOrder(app: ReturnType<typeof useApp>, entry: AudioEntry, order: readonly CodecId[]) {
  return saveEntry(app, entry, (base) => ({ ...base, codec_order: orderKeys(order) }));
}

/** The codec a device streams with now, when its profile streams (aptX
 *  Adaptive LL's card while aptX Adaptive streams as its profile chose it in
 *  Low Latency: not under Auto-select, which picks aptX Adaptive with its own
 *  settings) */
export function streamingCodec(snap: Snapshot, entry: AudioEntry): CodecId | null {
  const st = snap.stream;
  const p = entry.profile;
  if (st.state !== "streaming" || !st.info || !p || st.profile !== p.name) return null;
  const key = codecLabelKey(st.info.codec);
  const id = CODECS.find((c) => c.id !== "auto" && c.label === key)?.id ?? null;

  return id === "aptxad" && cardOf(p) === "aptxadll" && !snap.settings.auto_codec ? "aptxadll" : id;
}

/** The codec a device's cards (and Home's Codec card) show chosen: under
 *  Auto (Auto-select on, `autoAll`, or no codec chosen for the device yet)
 *  the one it streams with, else the one it would pick (null when the
 *  device's codecs are not known); else the codec chosen. `order`: its
 *  cards' order. */
export function pickedCodec(
  snap: Snapshot,
  entry: AudioEntry,
  autoAll: boolean,
  order: readonly CodecId[],
): string | null {
  const p = entry.profile ?? entryBase(entry, snap.profiles);
  const codec = cardOf(p);
  // Outside developer mode its codecs are left out, as the core does: one
  // chosen is used as one the device does not offer, never asked for anyway
  const developer = snap.settings.developer_mode;
  const known = offeredCodecs(knownCodecs(snap.devices, entry.address), developer);
  const blocked = !developer && developerOnly(codec);
  // One it may offer once chosen stays chosen (issue #30)
  if (!autoAll && !blocked && codecsOnRequest(snap.devices, entry.address).includes(codecKey(codec))) return codec;
  // One chosen on its card though the device does not offer it: that one;
  // one it no longer offers: the one used instead (issue #86)
  if (!autoAll && codec !== "auto") {
    if (blocked) return effectiveCodec(codec, known, order);
    return p.codec_forced ? codec : (effectiveCodec(codec, known, order) ?? codec);
  }
  return streamingCodec(snap, entry) ?? effectiveCodec("auto", known, order);
}

/** Whether a codec's card shows: the one picked always, the others unless
 *  `hide` and the device did not offer them (a codec chosen that it does
 *  not offer neither: the one used instead is picked). A device never
 *  connected shows SBC alone, the codec every device has (issue #86: an
 *  imported device's cards were a wall of unconfirmed ones). */
export function codecShown(
  id: string,
  picked: string | null,
  known: readonly string[],
  hide: boolean,
  onRequest: readonly string[] = [],
) {
  if (!hide || id === picked) return true;
  if (known.length === 0) return id === "sbc";
  return codecSupport(id, known, onRequest) !== "not_offered";
}

/** Whether `order` puts the cards in `ids` (those shown) in their default
 *  order; where the hidden ones are does not matter (issue #86) */
export function shownInDefaultOrder(order: readonly CodecId[], ids: readonly string[]): boolean {
  const mine = order.filter((c) => ids.includes(c));
  return CARD_ORDER.filter((c) => ids.includes(c)).every((c, i) => c === mine[i]);
}

export function Codecs() {
  const app = useApp();
  const { core, navigate } = app;
  const snap = () => core.state.snap!;
  const entries = () => audioEntries(snap());
  const [hide, setHideRaw] = createSignal(loadHide());
  const setHide = (v: boolean) => {
    setHideRaw(v);
    try {
      localStorage.setItem(HIDE_KEY, v ? "1" : "0");
    } catch {
      // Only remembered when the WebView keeps storage
    }
  };
  const autoAll = () => snap().settings.auto_codec;
  // A card, or its "...", clicked under Auto-select (issue #86 round 8):
  // what follows once it is turned off (null: the dialog is closed)
  const [locked, setLocked] = createSignal<(() => void) | null>(null);
  // The PC's output devices with their formats (the headset microphone's
  // output, Auto matching them in developer mode), again when the window
  // comes back (changed in Windows meanwhile)
  const [audioDevices, setAudioDevices] = createSignal<AudioDevice[]>([]);
  const loadDevices = () => void core.request("audio.devices").then(setAudioDevices, () => {});
  loadDevices();
  window.addEventListener("focus", loadDevices);
  onCleanup(() => window.removeEventListener("focus", loadDevices));
  onAudioDevicesChanged(loadDevices);

  return (
    <div class="page codecs">
      <header class="page-head">
        <h1>{t("ui.nav.codecs")}</h1>
      </header>
      <div class="codecs-options">
        <Toggle
          label={t("ui.codecs.auto_select")}
          checked={autoAll()}
          onChange={(v) => void saveSettings(app, { auto_codec: v })}
        />
        {/* "Show only supported codecs", on unless turned off (kept per
            WebView, under the key it always had) */}
        <Toggle
          label={t("ui.profile.codecs_hide")}
          checked={hide()}
          onChange={setHide}
        />
      </div>
      <Show
        when={entries().length > 0}
        fallback={
          <section class="card empty">
            <Icon name="headphones" class="empty-icon" />
            <p>{t("ui.home.no_device")}</p>
            <Button kind="strong" icon="add" onClick={() => navigate({ name: "devices" })}>
              {t("ui.home.add_device")}
            </Button>
          </section>
        }
      >
        {/* By key: a device keeps its section (and an open dialog) across updates */}
        <For each={entries().map((e) => e.key)}>
          {(key) => {
            const entry = entryByKey(entries, key);
            return (
              <DeviceCodecs
                entry={entry()}
                hide={hide()}
                auto={autoAll()}
                audioDevices={audioDevices()}
                onLocked={(then) => setLocked(() => then)}
              />
            );
          }}
        </For>
      </Show>
      <Dialog
        open={locked() !== null}
        title={t("ui.codecs.auto_dialog_title")}
        confirm={t("ui.codecs.auto_dialog_disable")}
        cancel={t("modal.cancel")}
        onConfirm={async () => {
          const then = locked();
          setLocked(null);
          // Then the card clicked is chosen, or its "..." opens (issue #86)
          if (await saveSettings(app, { auto_codec: false })) then?.();
        }}
        onCancel={() => setLocked(null)}
      >
        <p>{t("ui.codecs.auto_dialog_text")}</p>
      </Dialog>
    </div>
  );
}

/** One device: its activity, name, address and tags, then its codec cards */
function DeviceCodecs(props: {
  entry: AudioEntry;
  hide: boolean;
  auto: boolean;
  /** The PC's output devices with their formats (read only while Auto
   *  matches them) */
  audioDevices: readonly AudioDevice[];
  /** A card clicked while Auto-select is on: `then` once it is turned off */
  onLocked: (then: () => void) => void;
}) {
  const app = useApp();
  const snap = () => app.core.state.snap!;
  const e = () => props.entry;
  const [busy, setBusy] = createSignal(false);
  // The profile shown: the device's, or what it would start from
  const shown = (): Profile => e().profile ?? entryBase(e(), snap().profiles);
  // The format Windows plays its source in (one app: the default output, as
  // the core reads it)
  const sourceFormat = () => {
    const p = shown();
    const d =
      p.capture_mode === "app"
        ? props.audioDevices.find((x) => x.is_default)
        : captureDevice(p, props.audioDevices);
    return d && d.mix_rate !== null && d.mix_bits !== null ? { rate: d.mix_rate, bits: d.mix_bits } : null;
  };
  const known =() => knownCodecs(snap().devices, e().address);
  const onRequest = () => codecsOnRequest(snap().devices, e().address);
  const support = (id: string) => codecSupport(id, known(), onRequest());
  // On its card, SBC is supported before the first connection too: every
  // device has it (SBC XQ needs what the device lists, so not it)
  const shownSupport = (id: string) => (id === "sbc" && known().length === 0 ? "offered" : support(id));
  // The device's order while it is being dragged (null: the saved one)
  const [dragOrder, setDragOrder] = createSignal<CodecId[] | null>(null);
  // Its cards' order, which Auto follows under Auto-select too
  const order = (): CodecId[] => dragOrder() ?? cardOrder(shown().codec_order);
  const picked = () => pickedCodec(snap(), e(), props.auto, order());
  // Opus 05's card only in developer mode (issue #86); hidden, it keeps its
  // place in the order
  const cards = () =>
    offeredCodecs(order(), snap().settings.developer_mode)
      .filter((id) => codecShown(id, picked(), known(), props.hide, onRequest()))
      .map((id) => codecSpec(id));
  const [settingsOf, setSettingsOf] = createSignal<CodecSpec | null>(null);
  // A save that ends after the screen is gone starts no wait
  let mounted = true;
  onCleanup(() => (mounted = false));
  // The device's saves one after the other: a card chosen right after one
  // was moved starts from the profile with the new order
  let saving: Promise<unknown> = Promise.resolve();
  const queued = <T,>(save: () => Promise<T>): Promise<T> => {
    const done = saving.then(save);
    saving = done.catch(() => undefined);
    return done;
  };
  // The card clicked is checked by the browser: back to the one chosen
  // (nothing saved, or the save failed)
  const showPicked = () => {
    for (const r of grid?.querySelectorAll<HTMLInputElement>("input[type=radio]") ?? []) r.checked = r.value === picked();
  };
  // A card the device does not offer is asked for anyway (issue #86); the
  // codec chosen keeps how it was chosen when only its settings change
  const forcedFor = (id: string) =>
    id === cardOf(shown()) && id === picked() ? shown().codec_forced : support(id) === "not_offered";
  const choose = async (id: string, values?: Omit<CodecChoice, "codec">, live = false) => {
    // A "..." dialog's change while the cards wait: applied after (the cards
    // themselves are disabled meanwhile)
    if (settling()) await untilSettled();
    // The device's stream starts again with the codec: give it time first
    const running = state() !== "idle" && state() !== "error";
    setBusy(true);
    // An order moved but not back from the core yet goes with it
    const saved = await queued(() =>
      chooseCodec(app, e(), id, values, props.auto ? null : dragOrder(), forcedFor(id)),
    );
    setBusy(false);
    showPicked();
    if (saved && running && mounted && !live) settle(app);
  };
  // ---- a "..." dialog's changes (issue #86: applied at once) ----
  let pending: CodecChoice | null = null;
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  const apply = async ({ codec: id, ...values }: CodecChoice) => {
    const p = shown();
    const mine = id === cardOf(p);
    if (mine || id === picked()) await choose(id, values, mine && onlyBitpools(p, values));
    else
      await queued(() =>
        saveEntry(app, e(), (base) => ({ ...base, codec_settings: { ...base.codec_settings, [id]: values } })),
      );
  };
  const saveNow = () => {
    clearTimeout(saveTimer);
    const values = pending;
    pending = null;
    if (values) void apply(values);
  };
  onCleanup(saveNow);
  const locked = () => busy() || settling() || reopening() !== null;
  const titleId = () => `codecs-${e().key.replace(/[^0-9A-Za-z]/g, "")}`;
  const leSupport = () => snap().adapter.le_audio;
  const state = () => entryState(snap(), e());
  // The call link (HFP) open: its codecs instead of the music ones, which
  // do not play meanwhile (issue #86)
  const ours = () => snap().stream.state !== "idle" && snap().stream.profile === e().profile?.name;
  const callCodec = () => {
    const v = ours() ? snap().stream.voice : null;
    return v?.link === "hfp" ? v.codec : null;
  };
  // LE Audio: the microphone's rate while it runs (none: it does not)
  const leMicRate = () => {
    const v = ours() ? snap().stream.voice : null;
    return e().le_audio && v?.link === "le" ? v.rate : null;
  };
  const micUp = () => (e().le_audio ? leMicRate() !== null : callCodec() !== null);
  // The microphone's codec chosen (or Auto-select switched) closes the call
  // link and opens it again with A2DP in between, or sets the LE Audio
  // stream up again: its cards stay until it is back (issue #86: the music
  // cards flashed up meanwhile, the LE microphone's "..." closed). `to`:
  // the call codec shown chosen meanwhile; `rate`: the LE microphone's.
  const [reopening, setReopening] = createSignal<{ to: string; rate: number } | null>(null);
  let reopenTimer: ReturnType<typeof setTimeout> | undefined;
  let wentDown = false;
  const endReopen = () => {
    clearTimeout(reopenTimer);
    setReopening(null);
  };
  onCleanup(() => clearTimeout(reopenTimer));
  const awaitReopen = (to: string, rate = 0) => {
    clearTimeout(reopenTimer);
    wentDown = false;
    setReopening({ to, rate });
    // Not closed soon: nothing to wait for (it kept what it had)
    reopenTimer = setTimeout(() => {
      if (!wentDown) return endReopen();
      reopenTimer = setTimeout(endReopen, SETTLE_MAX_MS);
    }, 2000);
  };
  createEffect(() => {
    if (!reopening()) return;
    // A classic stream stays while its call link opens again: gone, it stopped
    if (!e().le_audio && !ours()) return endReopen();
    if (!micUp()) wentDown = true;
    else if (wentDown) endReopen();
  });
  const callOpen = () => !e().le_audio && (callCodec() !== null || reopening() !== null);
  // The rates the headphones allow, kept while the stream is set up again
  const [lastMicRates, setLastMicRates] = createSignal<readonly number[]>([]);
  createEffect(() => {
    const r = ours() ? snap().stream.mic_rates : [];
    if (r.length > 0) setLastMicRates(r);
  });
  const micRates = () => {
    const r = ours() ? snap().stream.mic_rates : [];
    return r.length > 0 || !reopening() ? r : lastMicRates();
  };
  const micRateShown = () => leMicRate() ?? (e().le_audio ? reopening()?.rate || null : null);
  const listedCalls = () => (ours() ? snap().stream.voice_codecs : []);
  // CVSD needs no listing: every headset has it
  const callSupported = (id: string) => id === "cvsd" || listedCalls().includes(id);
  const callCards = () => VOICE_CARDS.filter((c) => c.id === callCodec() || callSupported(c.id) || !props.hide);
  // Like the music cards: tagged only while the unsupported ones show
  const callSupport = (id: string): CodecSupport | null =>
    props.hide ? null : callSupported(id) ? "offered" : "not_offered";
  // Auto-select switched while the microphone runs with a codec chosen: it
  // opens again (Auto's or the one chosen, as the core)
  const micChosen = () => (e().le_audio ? shown().mic_rate !== 0 : shown().voice_codec !== "");
  createEffect(
    on(
      () => props.auto,
      () => {
        if (micUp() && micChosen()) awaitReopen(callCodec() ?? "", leMicRate() ?? 0);
      },
      { defer: true },
    ),
  );
  // The microphone's codec chosen (issue #86): the call link (or the LE
  // stream) opens again with it
  const saveMic = async (patch: Partial<Pick<Profile, "voice_codec" | "mic_rate">>) => {
    setBusy(true);
    if (micUp()) awaitReopen(patch.voice_codec ?? "", leMicRate() ?? 0);
    const ok = await queued(() => saveMicCodec(app, e(), patch));
    setBusy(false);
    if (!ok) endReopen();
  };
  const saveLowLatency = async (on: boolean) => {
    if (e().profile && shown().le_low_latency === on) return;
    setBusy(true);
    await queued(() => saveEntry(app, e(), (base) => ({ ...base, le_low_latency: on })));
    setBusy(false);
  };

  // ---- the device's order (issue #86 round 8) ----
  // Not `busy`: the card moved with the keyboard keeps the focus
  const [ordering, setOrdering] = createSignal(false);
  const hintId = createUniqueId();
  const [said, setSaid] = createSignal("");
  // A saved order replaces the one dragged
  createEffect(on(() => shown().codec_order.join(), () => setDragOrder(null), { defer: true }));
  const saveOrder = async (next: CodecId[]) => {
    // The cards shown back in their default order: the default order, the
    // hidden ones too (issue #86: no Reset for an order that changes nothing)
    if (shownInDefaultOrder(next, cards().map((c) => c.id))) next = [...CARD_ORDER];
    if (next.every((c, i) => c === cardOrder(shown().codec_order)[i])) {
      setDragOrder(null);
      return;
    }
    setDragOrder(next);
    setOrdering(true);
    const ok = await queued(() => saveCodecOrder(app, e(), next));
    setOrdering(false);
    if (!ok) setDragOrder(null);
  };
  // `id` put where `over` is in the full order (hidden cards keep their place)
  const placed = (list: CodecId[], id: CodecId, over: CodecId) => moved(list, list.indexOf(id), list.indexOf(over));
  let grid: HTMLDivElement | undefined;

  // ---- motion (issue #86 round 9) ----
  // The card dragged is lifted and follows the pointer, the others slide to
  // their new places (FLIP) instead of jumping, and the card let go settles
  // into its own; moved with the keyboard or reset, the cards slide too.
  // No sliding with prefers-reduced-motion (the card still follows the
  // pointer: that is the drag itself, not an animation).
  const cells = () => [...(grid?.querySelectorAll<HTMLElement>(".codec-cell[data-codec]") ?? [])];
  const cellOf = (id: CodecId) => cells().find((c) => c.dataset.codec === id);
  const still = () => window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
  const SLIDE = "codec-slide";
  const slide = (el: HTMLElement, dx: number, dy: number) => {
    if (dx || dy)
      el.animate?.([{ transform: `translate(${dx}px, ${dy}px)` }, { transform: "none" }], {
        id: SLIDE,
        duration: 200,
        easing: "cubic-bezier(0.2, 0.8, 0.2, 1)",
      });
  };
  const slides = (el: HTMLElement) => (el.getAnimations?.() ?? []).filter((a) => a.id === SLIDE);
  /** Runs `change` (a new order), then slides each card but `except` from
   *  where it was on screen (mid-slide too) to its new place */
  const flip = (change: () => void, except?: CodecId) => {
    const before = new Map(cells().map((c) => [c.dataset.codec, c.getBoundingClientRect()]));
    batch(change);
    if (still()) return;
    for (const c of cells()) {
      const from = before.get(c.dataset.codec);
      if (!from || c.dataset.codec === except) continue;
      for (const a of slides(c)) a.cancel();
      const to = c.getBoundingClientRect();
      slide(c, from.left - to.left, from.top - to.top);
    }
  };
  const focusCard = (id: CodecId) =>
    queueMicrotask(() => grid?.querySelector<HTMLInputElement>(`input[value="${id}"]`)?.focus());
  // Alt+arrow keys: next to the card before or after it among those shown
  const moveBy = (id: CodecId, by: -1 | 1) => {
    const shownIds = cards().map((c) => c.id);
    const at = shownIds.indexOf(id);
    const to = shownIds[at + by];
    if (!to) return;
    const next = placed(order(), id, to);
    setSaid(t("ui.codecs.moved", t(codecSpec(id).label), at + by + 1, shownIds.length));
    flip(() => void saveOrder(next));
    focusCard(id);
  };
  // Dragging with the pointer: the card follows it over the others and the
  // order is saved when it is let go (a click without moving still chooses).
  // `left` / `top`: the card's place in the grid when it was picked up.
  let drag: {
    id: CodecId;
    x: number;
    y: number;
    pointer: number;
    moving: boolean;
    left: number;
    top: number;
  } | null = null;
  let swallowClick = false;
  const [dragging, setDragging] = createSignal<CodecId | null>(null);
  // The card under the pointer (the one dragged lets the pointer through);
  // not one still sliding away, which would swap straight back
  const cellAt = (x: number, y: number): CodecId | null => {
    const el = document.elementFromPoint?.(x, y);
    const cell = el?.closest<HTMLElement>(".codec-cell[data-codec]");
    if (!cell || !grid?.contains(cell)) return null;
    return slides(cell).some((a) => a.playState === "running") ? null : (cell.dataset.codec as CodecId);
  };
  // The card dragged under the pointer, wherever its place in the grid is now
  const follow = (ev: PointerEvent) => {
    const el = drag && cellOf(drag.id);
    if (!drag || !el) return;
    const dx = ev.clientX - drag.x - (el.offsetLeft - drag.left);
    const dy = ev.clientY - drag.y - (el.offsetTop - drag.top);
    el.style.transform = `translate(${dx}px, ${dy}px)`;
  };
  const onPointerDown = (ev: PointerEvent) => {
    if (locked() || ev.button !== 0) return;
    const target = ev.target as Element;
    const cell = target.closest<HTMLElement>(".codec-cell[data-codec]");
    if (!cell || target.closest(".codec-more")) return;
    drag = {
      id: cell.dataset.codec as CodecId,
      x: ev.clientX,
      y: ev.clientY,
      pointer: ev.pointerId,
      moving: false,
      left: cell.offsetLeft,
      top: cell.offsetTop,
    };
  };
  const onPointerMove = (ev: PointerEvent) => {
    if (!drag || ev.pointerId !== drag.pointer) return;
    if (!drag.moving) {
      if (Math.hypot(ev.clientX - drag.x, ev.clientY - drag.y) < 6) return;
      drag.moving = true;
      setDragging(drag.id);
      grid?.setPointerCapture?.(ev.pointerId);
    }
    ev.preventDefault();
    const id = drag.id;
    const over = cellAt(ev.clientX, ev.clientY);
    if (over && over !== id) flip(() => setDragOrder(placed(order(), id, over)), id);
    follow(ev);
  };
  const endDrag = (ev: PointerEvent, cancelled: boolean) => {
    if (!drag || ev.pointerId !== drag.pointer) return;
    const d = drag;
    drag = null;
    if (!d.moving) return;
    // The click that ends a drag does not choose the card
    swallowClick = true;
    setTimeout(() => (swallowClick = false), 0);
    const el = cellOf(d.id);
    const from = el?.getBoundingClientRect();
    flip(() => {
      setDragging(null);
      if (cancelled) setDragOrder(null);
      else {
        // Where it is let go, if the last move did not get there
        const over = cellAt(ev.clientX, ev.clientY);
        if (over && over !== d.id) setDragOrder(placed(order(), d.id, over));
        const next = order();
        const at = cards().findIndex((c) => c.id === d.id);
        setSaid(t("ui.codecs.moved", t(codecSpec(d.id).label), at + 1, cards().length));
        void saveOrder(next);
      }
    }, d.id);
    // It settles from where it was let go into its place
    if (!el || !from) return;
    el.style.transform = "";
    if (still()) return;
    const to = el.getBoundingClientRect();
    slide(el, from.left - to.left, from.top - to.top);
  };

  return (
    <section
      class="card device-codecs"
      aria-labelledby={titleId()}
      aria-busy={busy() || ordering() ? "true" : undefined}
    >
      <div class="device-codecs-head">
        {/* Which device is in use (issue #86 round 8), as the sidebar's dot;
            never red here, it looked like recording (issue #86) */}
        <span class={`dot st-${state() === "error" ? "idle" : state()}`} role="img" aria-label={stateLabel(state())} title={stateLabel(state())} />
        <Icon name="headphones" />
        <div class="device-codecs-title">
          <h2 id={titleId()} class="clamp-2" ref={titleWhenCut(() => e().name || t("device.name_unknown"))}>
            {e().name || t("device.name_unknown")}
          </h2>
        </div>
        {/* No "Paired": that is the Audio Devices page's (issue #86) */}
        <span class="tags">
          {/* LE Audio: that tag alone (issue #86) */}
          <Show when={e().device.audio && !e().le_audio}>
            <span class="tag">{t("device.tag_audio")}</span>
          </Show>
          <Show when={e().le_audio}>
            <span class="tag">{t("device.tag_le_audio")}</span>
          </Show>
        </span>
      </div>
      <Show
        when={!e().le_audio && !callOpen()}
        fallback={
          <>
            <FixedCards
              entry={e()}
              cards={e().le_audio ? LE_CARDS : callCards()}
              // LE Audio: LC3 unless LC3 LL is chosen (issue #86)
              picked={
                e().le_audio ? (shown().le_low_latency ? "lc3_ll" : "lc3") : (callCodec() ?? reopening()?.to ?? "")
              }
              disabled={locked()}
              // LE Audio: LC3 or its low latency configuration, from the
              // device's next connection, under Auto-select too; the call
              // codec only with Auto-select off (issue #86)
              readOnly={!e().le_audio && props.auto}
              onPick={(id) => void (e().le_audio ? saveLowLatency(id === "lc3_ll") : saveMic({ voice_codec: id }))}
              onLocked={(id) => props.onLocked(() => void saveMic({ voice_codec: id }))}
              mono={!e().le_audio}
              support={e().le_audio ? undefined : callSupport}
              note={
                <Show when={e().le_audio && leSupport() !== true}>
                  <Banner kind="warn">
                    {leSupport() === false ? t("ui.profile.le_audio_unsupported") : t("ui.profile.le_audio_unchecked")}
                  </Banner>
                </Show>
              }
            />
            {/* The LE Audio microphone while it runs: its rate in its "..." */}
            <Show when={micRateShown()}>
              {(rate) => (
                <>
                  <h3 class="codecs-sub">{t("ui.diag.mic_title")}</h3>
                  <FixedCards
                    entry={e()}
                    cards={[{ ...LE_MIC_CARD, rate: rate() }]}
                    picked={LE_MIC_CARD.id}
                    disabled={locked()}
                    readOnly
                    mono
                    rateChoice={
                      props.auto
                        ? undefined
                        : {
                            value: shown().mic_rate,
                            rates: micRates(),
                            onChange: (r) => void saveMic({ mic_rate: r }),
                          }
                    }
                  />
                </>
              )}
            </Show>
          </>
        }
      >
        <div
          ref={grid}
          class="codec-grid"
          classList={{ ordering: dragging() !== null }}
          role="radiogroup"
          aria-labelledby={titleId()}
          aria-disabled={props.auto ? "true" : undefined}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={(ev) => endDrag(ev, false)}
          onPointerCancel={(ev) => endDrag(ev, true)}
          onClick={(ev) => {
            if (!swallowClick) return;
            swallowClick = false;
            ev.preventDefault();
            ev.stopPropagation();
          }}
        >
          <For each={cards()}>
            {(c) => (
              <CodecCard
                spec={c}
                group={`codec-${e().key}`}
                selected={picked() === c.id}
                disabled={locked()}
                readOnly={props.auto}
                // Only supported codecs shown: no "Supported" tag on each
                support={props.hide && shownSupport(c.id) === "offered" ? null : shownSupport(c.id)}
                orderable
                dragging={dragging() === c.id}
                // The keys need a card that takes the focus (not under Auto-select)
                describedBy={props.auto ? undefined : hintId}
                onPick={() => void choose(c.id)}
                onReadOnly={(settings) => {
                  // The click that ends a drag
                  if (swallowClick) return;
                  // Developer mode: the codec's settings apply under Auto-select
                  if (settings && autoKeepsCodecSettings(snap().settings)) return setSettingsOf(c);
                  props.onLocked(() => (settings ? setSettingsOf(c) : void choose(c.id)));
                }}
                onSettings={() => setSettingsOf(c)}
                onMove={(by) => moveBy(c.id, by)}
              />
            )}
          </For>
        </div>
        <span id={hintId} class="sr-only">
          {t("ui.codecs.order_keys")}
        </span>
        <span class="sr-only" role="status" aria-live="polite">
          {said()}
        </span>
        {/* Visible hint for dragging (the cards' description above has the
            keys too); Reset beside it while the order is changed */}
        <Show when={cards().length > 1}>
          <div class="codecs-order">
            <p class="hint" aria-hidden="true">
              {t("ui.codecs.order_hint")}
            </p>
            <Show when={!shownInDefaultOrder(order(), cards().map((c) => c.id))}>
              <Button
                icon="arrow_reset"
                disabled={busy() || ordering()}
                onClick={() => flip(() => void saveOrder([...CARD_ORDER]))}
              >
                {t("ui.codecs.order_reset")}
              </Button>
            </Show>
          </div>
        </Show>
        <Show when={known().length === 0}>
          <p class="hint">{t("ui.profile.codecs_unknown")}</p>
        </Show>
        <CodecSettings
          spec={settingsOf()}
          profile={shown()}
          kept={(id) => keptFor(shown(), id)}
          sbcCaps={sbcCapsForSettings(knownSbcCaps(snap().devices, e().address), snap().settings)}
          caps={capsForSettings(knownCodecCaps(snap().devices, e().address), snap().settings)}
          developer={snap().settings.developer_mode}
          matchSource={snap().settings.developer_mode && snap().settings.dev_overrides.match_source}
          sourceFormat={sourceFormat()}
          deviceName={e().name}
          warnUnlisted={!snap().settings.unlisted_options_warned}
          onUnlistedWarned={() => void saveSettings(app, { unlisted_options_warned: true })}
          onForgetReport={() => void app.run(() => app.core.request("device.forget_codecs", { address: e().address }))}
          onChange={(values) => {
            pending = values;
            clearTimeout(saveTimer);
            saveTimer = setTimeout(saveNow, SAVE_DELAY_MS);
          }}
          onClose={() => {
            saveNow();
            setSettingsOf(null);
          }}
        />
      </Show>
    </section>
  );
}

/** A card that only shows its codec, with its settings read only (issue
 *  #86): the call codecs and LE Audio's */
type FixedCard = CardSpec & {
  /** Sample rate (Hz) and bit rate (kbps) when fixed */
  rate?: number;
  kbps?: number;
  /** No bit rate shown (not known) */
  noBitRate?: boolean;
};

/** The call codecs of a classic device's microphone link (HFP), highest
 *  quality first. The link chooses the best both sides have (issue #86). */
export const VOICE_CARDS: readonly FixedCard[] = [
  { id: "lc3_swb", label: "codec.lc3_swb", bars: { quality: 3, latency: 4, stability: 3 }, rate: 32000, kbps: 64 },
  { id: "msbc", label: "codec.msbc", bars: { quality: 2, latency: 4, stability: 4 }, rate: 16000, kbps: 64 },
  { id: "cvsd", label: "codec.cvsd", bars: { quality: 1, latency: 4, stability: 5 }, rate: 8000, kbps: 64 },
];

/** The LE Audio microphone: LC3, one channel, at the rate it runs at */
export const LE_MIC_CARD: FixedCard = {
  id: "lc3_mic",
  label: "codec.lc3",
  bars: { quality: 3, latency: 4, stability: 4 },
  noBitRate: true,
};

/** LE Audio's: LC3 as the device offers it best (the default), or its low
 *  latency configuration (the profile's le_low_latency) */
export const LE_CARDS: readonly FixedCard[] = [
  { id: "lc3", label: "codec.lc3", bars: { quality: 4, latency: 3, stability: 4 } },
  { id: "lc3_ll", label: "codec.lc3_ll", bars: { quality: 3, latency: 5, stability: 3 } },
];

/** Cards like the A2DP ones whose "..." shows the codec's settings read
 *  only. `onPick`: the card chosen; `readOnly`: the cards only show which
 *  one is in use (Auto-select on; `onLocked`: a card clicked then).
 *  `rateChoice`: the sample rate is chosen in "..." (the LE Audio
 *  microphone's, 0 = Auto, Auto's rate first in `rates`). */
function FixedCards(props: {
  entry: AudioEntry;
  cards: readonly FixedCard[];
  picked: string;
  disabled: boolean;
  readOnly?: boolean;
  onPick?: (id: string) => void;
  onLocked?: (id: string) => void;
  /** One channel: shown in "..." */
  mono?: boolean;
  rateChoice?: { value: number; rates: readonly number[]; onChange: (rate: number) => void };
  /** Each card's tag (none: untagged) */
  support?: (id: string) => CodecSupport | null;
  note?: JSX.Element;
}) {
  const app = useApp();
  const titleId = createUniqueId();
  const [open, setOpen] = createSignal<FixedCard | null>(null);
  // LE Audio's rate and bit rate are the stream's, while it plays
  const info = () => {
    const st = app.core.state.snap!.stream;
    return st.state === "streaming" && st.profile === props.entry.profile?.name ? st.info : null;
  };
  const rate = (c: FixedCard) => c.rate ?? (c.id === props.picked ? info()?.sample_rate : undefined);
  const kbps = (c: FixedCard) => c.kbps ?? (c.id === props.picked ? info()?.bitrate_kbps : undefined);
  // The rates to choose between: Auto with the one it takes, then each
  // the headphones allow (one chosen they no longer allow stays, marked);
  // none while not known or only one
  const rateOptions = () => {
    const r = props.rateChoice;
    if (!r || r.rates.length < 2) return null;
    const label = (hz: number) => `${hz} Hz`;
    const listed = r.value === 0 || r.rates.includes(r.value) ? [...r.rates] : [...r.rates, r.value];
    return [
      { value: 0, label: t("codec.auto_value", label(r.rates[0])) },
      ...listed.map((hz) => ({
        value: hz,
        label: r.rates.includes(hz) ? label(hz) : t("ui.profile.sbc_not_offered", label(hz)),
      })),
    ];
  };
  const value = (id: string, text: string, described?: string) => (
    <output id={id} class="input readonly num" aria-describedby={described}>
      {text}
    </output>
  );
  return (
    <>
      <div class="codec-grid" role="radiogroup" aria-labelledby={titleId} aria-label={props.entry.name}>
        <For each={props.cards}>
          {(c) => (
            <CodecCard
              spec={c}
              group={`fixed-${props.entry.key}-${props.cards[0]?.id ?? ""}`}
              selected={props.picked === c.id}
              disabled={props.disabled}
              readOnly={props.readOnly || !props.onPick}
              support={props.support?.(c.id) ?? null}
              onPick={() => props.onPick?.(c.id)}
              onReadOnly={(settings) => (settings ? setOpen(c) : props.onLocked?.(c.id))}
              onSettings={() => setOpen(c)}
            />
          )}
        </For>
      </div>
      {props.note}
      <Dialog
        open={open() !== null}
        title={open() ? t("ui.profile.codec_settings", t(open()!.label)) : ""}
        onCancel={() => setOpen(null)}
      >
        <Show when={open()}>
          {(c) => (
            <div class="codec-settings">
              {/* As an A2DP codec's settings, read only, with the same
                  descriptions (issue #86) */}
              <Show when={!c().noBitRate}>
                <Field label={t("connection.quality")} hint={t("tooltip.quality")}>
                  {(id, described) => value(id, kbps(c()) ? `${kbps(c())} kbps` : "—", described)}
                </Field>
              </Show>
              <Show
                when={rateOptions()}
                fallback={
                  <Field label={t("connection.sample_rate")} hint={t("tooltip.sample_rate")}>
                    {(id, described) => value(id, rate(c()) ? `${rate(c())} Hz` : "—", described)}
                  </Field>
                }
              >
                {(options) => (
                  <Field label={t("connection.sample_rate")} hint={t("tooltip.sample_rate")}>
                    {(id, described) => (
                      <Select
                        id={id}
                        describedBy={described}
                        value={props.rateChoice!.value}
                        options={options()}
                        disabled={props.disabled}
                        onChange={(r) => props.rateChoice!.onChange(r)}
                      />
                    )}
                  </Field>
                )}
              </Show>
              {/* LC3 and the call codecs carry 16-bit samples */}
              <Field label={t("connection.bit_depth")} hint={t("tooltip.bit_depth")}>
                {(id, described) => value(id, t("bitdepth.16"), described)}
              </Field>
              {/* The call link and the microphone are one channel */}
              <Show when={props.mono}>
                <Field label={t("ui.diag.cd_channel_mode")} hint={t("tooltip.channel_mode")}>
                  {(id, described) => value(id, t("ui.diag.cd_mode.mono"), described)}
                </Field>
              </Show>
            </div>
          )}
        </Show>
      </Dialog>
    </>
  );
}
