// The codecs a profile can ask for and what each allows, as v1's profile
// dialog (wx_profile_dialog.cpp on the develop branch, update_codec_dependent): quality
// modes with their bitrates (since issue #86 High / Medium / Low for LDAC
// and LHDC V5, LHDC V5's Medium-High too (issue #203), High / Low Latency for aptX Adaptive, only High, the highest
// bit rate the device takes, for the others; core: Quality::for_codec),
// ABR only for LDAC / LHDC V5 (and Auto, which may
// pick them; v2 adds SBC / SBC XQ's adaptive bitpool), 24-bit and 88.2 / 96 kHz only for LDAC and LHDC V5, 192 kHz and
// "mhq" only for LHDC V5, no quality choice for the aptX family (but aptX
// Adaptive) or under LHDC V5 ABR, 48 kHz only for Opus and Opus 05, 48 / 96
// kHz for aptX Adaptive, whose "ll" quality is its low latency mode (on
// the Codecs screen a card of its own, aptX Adaptive LL: issue #86).
// aptX HD and aptX Adaptive always carry 24-bit (no choice).

import type { Settings } from "../ipc/types";
import type { SbcCaps } from "./sbc";

export type CodecId =
  | "auto"
  | "ldac"
  | "aptxhd"
  | "aptxll"
  | "aptx"
  | "aptxad"
  | "aptxadll"
  | "lhdcv5"
  | "opus"
  | "aac"
  | "sbcxq"
  | "sbc"
  | "faststream"
  | "opus05";

export interface QualityMode {
  id: string;
  /** Translation key of the name */
  label: string;
  kbps?: string;
}

export interface CodecSpec {
  id: CodecId;
  /** Translation key of the name */
  label: string;
  /** 1..5 for the comparison bars (Auto has none: it depends on the device) */
  bars?: { quality: number; latency: number; stability: number };
  /** Quality modes; empty: the codec has a fixed bitrate */
  qualities: QualityMode[];
  abr: boolean;
  /** 24-bit and 88.2 / 96 kHz */
  hiRes: boolean;
  /** The only capture rates it takes (0 = auto), when not the usual ones */
  rates?: number[];
  /** Its one bit rate at 44.1 / 48 kHz when it has no qualities (core:
   *  Encoder::bitrate_kbps) */
  fixedKbps?: [number, number];
  /** The bit depth it always carries when there is no choice (16 if not set) */
  bits?: number;
  /** Variable bit rate (where the headphones list it), on the "abr" setting */
  vbr?: boolean;
  /** The SBC bitpool and frame setup (blocks, subbands, allocation) can be chosen */
  bitpool?: boolean;
  /** Shown and used only in developer mode (core: CodecKind::developer_only):
   *  outside it, a device's is left out as if it did not offer it */
  developer?: boolean;
}

/** SBC's largest bitpool (the local SEP's) */
export const BITPOOL_MAX = 53;
/** SBC XQ is dual channel: its largest bitpool per channel (44.1 kHz; 43 at 48 kHz) */
export const BITPOOL_MAX_XQ = 47;
export const BITPOOL_MIN = 2;

const HQ = "quality.high";
const MEDIUM_HIGH = "quality.medium_high";
const MEDIUM = "quality.medium";
const LOW = "quality.low";
const LL = "quality.low_latency";

export const CODECS: CodecSpec[] = [
  {
    id: "auto",
    label: "codec.auto",
    // Auto-select takes the best options (issue #86)
    qualities: [{ id: "hq", label: HQ }],
    abr: true,
    hiRes: false,
  },
  {
    id: "ldac",
    label: "codec.ldac",
    bars: { quality: 5, latency: 2, stability: 2 },
    qualities: [
      { id: "hq", label: HQ, kbps: "990" },
      { id: "sq", label: MEDIUM, kbps: "660" },
      { id: "mq", label: LOW, kbps: "330" },
    ],
    abr: true,
    hiRes: true,
  },
  {
    id: "lhdcv5",
    label: "codec.lhdc_v5",
    bars: { quality: 5, latency: 2, stability: 2 },
    // Android's four fixed levels (the sink may cap the maximum). Medium-
    // High was left out in issue #86 and is back since issue #203
    qualities: [
      { id: "hq", label: HQ, kbps: "1000" },
      { id: "mhq", label: MEDIUM_HIGH, kbps: "900" },
      { id: "sq", label: MEDIUM, kbps: "500" },
      { id: "mq", label: LOW, kbps: "400" },
    ],
    abr: true,
    hiRes: true,
    // No 88.2 kHz: the device lists 44.1, 48, 96 and 192 kHz only (issue #86)
    rates: [0, 44100, 48000, 96000, 192000],
  },
  {
    id: "aptxhd",
    label: "codec.aptx_hd",
    bars: { quality: 4, latency: 3, stability: 3 },
    qualities: [],
    fixedKbps: [529, 576],
    abr: false,
    hiRes: false,
    bits: 24,
  },
  {
    id: "aptx",
    label: "codec.aptx",
    bars: { quality: 3, latency: 3, stability: 4 },
    qualities: [],
    fixedKbps: [352, 384],
    abr: false,
    hiRes: false,
  },
  {
    id: "aptxll",
    label: "codec.aptx_ll",
    bars: { quality: 2, latency: 5, stability: 4 },
    qualities: [],
    fixedKbps: [352, 384],
    abr: false,
    hiRes: false,
  },
  {
    // aptX Adaptive: High with an R1 sink
    // (an R2 sink: 427 -> 340), 48 kHz; ABR steps down from High; Low
    // Latency has a fixed bit rate. 96 kHz when chosen, to a device that
    // lists it (an R2 one), not in Low Latency (48 kHz otherwise; 646
    // kbps). Always 24-bit.
    id: "aptxad",
    label: "codec.aptx_adaptive",
    bars: { quality: 4, latency: 3, stability: 4 },
    qualities: [
      { id: "hq", label: HQ, kbps: "427" },
      { id: "ll", label: LL, kbps: "265" },
    ],
    abr: true,
    hiRes: false,
    rates: [0, 48000, 96000],
    bits: 24,
  },
  {
    id: "opus",
    label: "codec.opus",
    bars: { quality: 4, latency: 3, stability: 4 },
    // Android's A2DP Opus: 256 kbps stereo (mono: half), 48 kHz, 20 ms frames
    qualities: [{ id: "hq", label: HQ, kbps: "256" }],
    abr: false,
    hiRes: false,
    rates: [0, 48000],
  },
  {
    id: "aac",
    label: "codec.aac",
    bars: { quality: 3, latency: 2, stability: 4 },
    qualities: [
      { id: "hq", label: HQ, kbps: "256" },
      { id: "sq", label: MEDIUM, kbps: "192" },
      { id: "mq", label: LOW, kbps: "128" },
    ],
    abr: false,
    vbr: true,
    hiRes: false,
  },
  {
    // Dual channel, high bitpool (PipeWire's SBC-XQ); only on sinks whose
    // SBC allows it
    id: "sbcxq",
    label: "codec.sbc_xq",
    bars: { quality: 3, latency: 3, stability: 3 },
    // High: the highest bitpool the device takes (or the one set)
    qualities: [
      { id: "hq", label: HQ, kbps: "~552" },
      { id: "sq", label: MEDIUM, kbps: "~512" },
      { id: "mq", label: LOW, kbps: "~385" },
    ],
    // Adaptive bitpool, down to Mobile's (core: SBC ABR only when the
    // profile asks for SBC / SBC XQ itself)
    abr: true,
    bitpool: true,
    hiRes: false,
  },
  {
    id: "sbc",
    label: "codec.sbc",
    bars: { quality: 2, latency: 3, stability: 5 },
    qualities: [
      { id: "hq", label: HQ, kbps: "~345" },
      { id: "sq", label: MEDIUM, kbps: "~229" },
      { id: "mq", label: LOW, kbps: "~133" },
    ],
    abr: true,
    bitpool: true,
    hiRes: false,
  },
  // CSR FastStream: fixed SBC (bitpool 29, ~200 kbps), music only; never
  // picked by Auto
  {
    id: "faststream",
    label: "codec.faststream",
    bars: { quality: 1, latency: 4, stability: 4 },
    qualities: [],
    fixedKbps: [198, 216],
    abr: false,
    hiRes: false,
  },
  {
    // PipeWire's own Opus ("Opus 05"; never picked by Auto):
    // Android Opus' levels, stereo, 48 kHz, 10 ms frames when offered
    id: "opus05",
    label: "codec.opus05",
    bars: { quality: 4, latency: 3, stability: 3 },
    qualities: [{ id: "hq", label: HQ, kbps: "256" }],
    abr: false,
    hiRes: false,
    rates: [0, 48000],
    developer: true,
  },
];

/**
 * aptX Adaptive's low latency mode as a card of its own (issue #86), the
 * codec cards' only one that is not a codec: chosen, the profile asks for
 * aptX Adaptive with quality "ll" (what earlier versions saved for it
 * loads as this card). 48 kHz, 24-bit, its one bit rate; no ABR.
 */
export const APTXAD_LL: CodecSpec = {
  id: "aptxadll",
  label: "codec.aptx_adaptive_ll",
  bars: { quality: 3, latency: 5, stability: 3 },
  qualities: [],
  fixedKbps: [265, 265],
  abr: false,
  hiRes: false,
  rates: [0, 48000],
  bits: 24,
};

export function codecSpec(id: string): CodecSpec {
  if (id === APTXAD_LL.id) return APTXAD_LL;
  return CODECS.find((c) => c.id === id) ?? CODECS[0];
}

/** The codec a card is (what the device lists and the core streams):
 *  aptX Adaptive LL's is aptX Adaptive */
export function codecKey(card: string): string {
  return card === APTXAD_LL.id ? "aptxad" : card;
}

/** The card a profile's codec shows on: aptX Adaptive in Low Latency is
 *  aptX Adaptive LL's */
export function cardOf(p: { codec: string; quality: string }): string {
  return p.codec === "aptxad" && p.quality === "ll" ? APTXAD_LL.id : p.codec;
}

/** A profile's codec and quality for a card: aptX Adaptive LL's are aptX
 *  Adaptive and Low Latency, aptX Adaptive's own never Low Latency */
export function cardCodec<T extends { codec: string; quality: string }>(p: T): T {
  if (p.codec === APTXAD_LL.id) return { ...p, codec: "aptxad", quality: "ll" };
  if (p.codec === "aptxad" && p.quality === "ll") return { ...p, quality: "hq" };
  return p;
}

/** Translation key of a codec name as the core reports it ("LDAC", "aptX HD") or a profile id */
export function codecLabelKey(name: string): string {
  const n = name.toLowerCase().replace(/[\s_-]/g, "");
  const map: Record<string, string> = {
    auto: "codec.auto",
    ldac: "codec.ldac",
    aptx: "codec.aptx",
    aptxhd: "codec.aptx_hd",
    aptxll: "codec.aptx_ll",
    aptxlowlatency: "codec.aptx_ll",
    aptxad: "codec.aptx_adaptive",
    aptxadaptive: "codec.aptx_adaptive",
    aac: "codec.aac",
    sbc: "codec.sbc",
    sbcxq: "codec.sbc_xq",
    faststream: "codec.faststream",
    lhdcv5: "codec.lhdc_v5",
    lhdc: "codec.lhdc_v5",
    opus: "codec.opus",
    opus05: "codec.opus05",
    // LE Audio (StreamInfo.le)
    lc3: "ui.codec.lc3",
  };
  return map[n] ?? "";
}

/**
 * Whether a device offers a codec, from the codec keys the core learnt when
 * it last connected (Device.codecs: "ldac", "aac", ...; empty: not known
 * until the first connection). Auto takes what the device offers. One it may
 * offer only once chosen (`onRequest`, Device.codecs_on_request) is not known
 * until then (issue #30).
 */
export type CodecSupport = "offered" | "not_offered" | "unknown";

export function codecSupport(codec: string, known: readonly string[], onRequest: readonly string[] = []): CodecSupport {
  if (known.length === 0) return "unknown";
  if (codec === "auto") return "offered";
  const id = codecKey(codec).toLowerCase();
  if (known.some((k) => k.toLowerCase() === id)) return "offered";
  return onRequest.includes(id) ? "unknown" : "not_offered";
}

/** Developer mode's override: each codec's "..." settings apply under
 *  Auto-select too, and open without turning it off */
export function autoKeepsCodecSettings(s: Pick<Settings, "developer_mode" | "dev_overrides">): boolean {
  return s.developer_mode && s.dev_overrides.auto_codec_settings;
}

/** Developer mode's override: every codec runs at its quality's bit rate,
 *  whatever maximum the device lists (core: DevOverrides::ignore_sink_max) */
export function ignoresSinkMax(s: Pick<Settings, "developer_mode" | "dev_overrides">): boolean {
  return s.developer_mode && s.dev_overrides.ignore_sink_max;
}

/** What the codec settings go by: without the device's maximum bit rates
 *  while developer mode's override ignores them, so every quality is offered */
export function capsForSettings(
  caps: readonly CodecCaps[],
  s: Pick<Settings, "developer_mode" | "dev_overrides">,
): readonly CodecCaps[] {
  if (!ignoresSinkMax(s)) return caps;
  return caps.map((c) => (c.max_bitrate > 0 ? { ...c, max_bitrate: 0 } : c));
}

/** The SBC setup the codec settings go by: no device maximum bitpool while
 *  the override ignores it (the device's minimum still applies, as in the
 *  core) */
export function sbcCapsForSettings(
  caps: SbcCaps | null,
  s: Pick<Settings, "developer_mode" | "dev_overrides">,
): SbcCaps | null {
  if (!caps || caps.max_bitpool < 2 || !ignoresSinkMax(s)) return caps;
  return { ...caps, max_bitpool: 255 };
}

/**
 * Auto's choice, highest sound quality first (core: CodecKind::AUTO_PRIORITY,
 * issue #86). FastStream and Opus 05 are used only when chosen.
 */
export const AUTO_ORDER: readonly CodecId[] = [
  "lhdcv5",
  "ldac",
  "aptxad",
  "opus",
  "aptxhd",
  "aptx",
  "aptxll",
  "aac",
  "sbcxq",
  "sbc",
];

/**
 * The codec keys of `keys` that Auto and the fallback take (those of
 * AUTO_ORDER) in the order given, then any missing in their default order
 * (core: AutoOrder::from_keys). Empty: AUTO_ORDER, highest sound quality
 * first.
 */
export function autoOrder(keys: readonly string[] = []): CodecId[] {
  const order: CodecId[] = [];
  for (const k of [...keys.map((k) => k.toLowerCase()), ...AUTO_ORDER]) {
    if (AUTO_ORDER.includes(k as CodecId) && !order.includes(k as CodecId)) order.push(k as CodecId);
  }
  return order;
}

/**
 * The default order of a device's codec cards: AUTO_ORDER, with Opus 05
 * beside Opus and FastStream last (Auto and the fallback never take those
 * two)
 */
export const CARD_ORDER: readonly CodecId[] = [
  "lhdcv5",
  "ldac",
  "aptxad",
  "aptxadll",
  "opus",
  "opus05",
  "aptxhd",
  "aptx",
  "aptxll",
  "aac",
  "sbcxq",
  "sbc",
  "faststream",
];

/**
 * A device's codec cards in its order (the profile's codec_order, issue #86
 * round 8: dragged with Auto-select off): the codecs given (each once,
 * unknown keys left out), then any missing in CARD_ORDER's order (as the
 * core's AutoOrder::from_keys). Empty: CARD_ORDER.
 */
export function cardOrder(keys: readonly string[] = []): CodecId[] {
  const given = keys.map((k) => k.toLowerCase());
  const order: CodecId[] = [];
  for (const k of given) {
    if (CARD_ORDER.includes(k as CodecId) && !order.includes(k as CodecId)) order.push(k as CodecId);
  }
  // aptX Adaptive LL, new in an order saved before it: beside aptX Adaptive
  if (!order.includes("aptxadll") && order.includes("aptxad")) order.splice(order.indexOf("aptxad") + 1, 0, "aptxadll");
  for (const k of CARD_ORDER) if (!order.includes(k)) order.push(k);
  return order;
}

/** The cards' order is the default one */
export function isDefaultOrder(keys: readonly string[]): boolean {
  return cardOrder(keys).every((c, i) => c === CARD_ORDER[i]);
}

/** `list` with the item at `from` moved to `to` */
export function moved<T>(list: readonly T[], from: number, to: number): T[] {
  const next = [...list];
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item);
  return next;
}

/** Shown and used only in developer mode (CodecSpec.developer) */
export function developerOnly(codec: string): boolean {
  return codecSpec(codec).developer === true;
}

/** The codecs of `list` the app offers: those of developer mode only with it on */
export function offeredCodecs<T extends string>(list: readonly T[], developer: boolean): T[] {
  return list.filter((c) => developer || !developerOnly(c));
}

/** Offered by the device */
function usable(codec: string, known: readonly string[]): boolean {
  return codecSupport(codec, known) === "offered";
}

/** The codec Auto picks among those a device offers (`known`, as
 *  Device.codecs): the first in `order` (the device's, AUTO_ORDER unless
 *  given; never FastStream nor Opus 05); null when not known */
export function autoPick(known: readonly string[], order: readonly string[] = AUTO_ORDER): CodecId | null {
  return autoOrder(order).find((c) => usable(c, known)) ?? null;
}

/**
 * The codec a connection uses for `codec` chosen on a device that offers
 * `known` (core: caps::choose_in with the fallback every profile has): the
 * codec itself when offered, else the first in the device's `order`
 * (AUTO_ORDER unless given) it offers, as Auto picks (issue #86 round 8:
 * not the next one below the codec chosen). Null when the device's codecs
 * are not known yet.
 */
export function effectiveCodec(
  codec: string,
  known: readonly string[],
  order: readonly string[] = AUTO_ORDER,
): CodecId | null {
  if (known.length === 0) return null;
  if (codec !== "auto" && usable(codec, known)) return codec as CodecId;
  return autoPick(known, order);
}

export function sampleRates(codec: string): number[] {
  const spec = codecSpec(codec);
  return spec.rates ?? (spec.hiRes ? [0, 44100, 48000, 88200, 96000] : [0, 44100, 48000]);
}

/** What a device's endpoint of a codec lists (Device.codec_caps) */
export interface CodecCaps {
  codec: string;
  sample_rates: number[];
  bit_depths: number[];
  max_bitrate: number;
  /** LHDC V5: it lists low latency (absent from an older core: not known) */
  lhdcv5_low_latency?: boolean;
}

/** What a codec's settings go by with its card's "Show only supported
 *  options" off (issue #86): everything the codec has, as when the device's
 *  caps are not known (core: RemoteCaps::unlisted). aptX Adaptive's 96 kHz
 *  stays the device's: it needs an R2 bitstream */
export function unlistedCaps(codec: string, caps: CodecCaps | null): CodecCaps | null {
  return codecKey(codec) === "aptxad" ? caps : null;
}

/** The caps of `codec` among a device's (null: not known) */
export function capsOf(caps: readonly CodecCaps[], codec: string): CodecCaps | null {
  const key = codecKey(codec).toLowerCase();
  return caps.find((c) => c.codec.toLowerCase() === key) ?? null;
}

/** Whether the device lists a sample rate (anything goes while not known).
 *  aptX Adaptive's 96 kHz only when
 *  the device lists it: few do (an R2 one with a 96 kHz bit; issue #86, a
 *  Bose QC Ultra has none), and the core streams 48 kHz to the others */
export function rateOffered(codec: string, rate: number, caps: CodecCaps | null): boolean {
  if (rate === 0) return true;
  if (codec === "aptxad" && rate === 96000) return caps?.sample_rates.includes(rate) ?? false;
  if (!caps || caps.sample_rates.length === 0) return true;
  return caps.sample_rates.includes(rate);
}

/** The one value a setting can take when there is nothing to choose (Auto
 *  could only pick it): shown as a fixed value, without Auto. Undefined when
 *  there is a choice, or when the value kept is one the device did not list
 *  (the list stays, to move off it) */
export function onlyChoice<T>(offered: readonly T[], current: T, auto: T): T | undefined {
  const values = offered.filter((x) => x !== auto);
  return values.length === 1 && (current === auto || current === values[0]) ? values[0] : undefined;
}

/** The sample rates the dialog offers (issue #86): the codec's, as far as
 *  the device lists them */
export function offeredSampleRates(codec: string, caps: CodecCaps | null): number[] {
  return sampleRates(codec).filter((r) => rateOffered(codec, r, caps));
}

/** The sample rate Auto streams `codec` at, shown beside "Auto" (issue
 *  #86), as the core picks it (stream.rs): 48 kHz, else what the device
 *  lists, 44.1 kHz first; SBC, SBC XQ and Opus always 48 kHz */
export function autoSampleRate(codec: string, caps: CodecCaps | null): number {
  if (["sbc", "sbcxq", "opus", "opus05"].includes(codec)) return 48000;
  const listed = offeredSampleRates(codec, caps).filter((r) => r > 0);
  if (listed.length === 0 || listed.includes(48000)) return 48000;
  return listed.includes(44100) ? 44100 : listed[0];
}

/** The bit depth Auto streams `codec` with (stream.rs): 24-bit for LDAC,
 *  aptX HD and aptX Adaptive, LHDC V5 when the device lists it, else 16 */
export function autoBitDepth(codec: string, caps: CodecCaps | null): number {
  if (codec === "ldac" || codec === "aptxhd" || codecKey(codec) === "aptxad") return 24;
  if (codec === "lhdcv5") {
    const listed = caps?.bit_depths ?? [];
    return listed.length === 0 || listed.includes(24) ? 24 : 16;
  }
  return 16;
}

/** Of `listed` (lowest first), the rate nearest the PC's `source` rate, as
 *  the core picks it (caps.rs nearest_rate): itself, else its family (44.1
 *  kHz multiples or not) below, then above; else the other family above,
 *  then below */
export function nearestRate(listed: readonly number[], source: number): number {
  const family = (r: number) => r % 11025 === 0;
  const same = listed.filter((r) => family(r) === family(source));
  const other = listed.filter((r) => family(r) !== family(source));
  const below = (rs: number[]) => rs.filter((r) => r <= source).at(-1);
  const above = (rs: number[]) => rs.find((r) => r >= source);
  return below(same) ?? above(same) ?? above(other) ?? below(other) ?? source;
}

/** The sample rate Auto streams `codec` at while it matches the PC's
 *  output (developer mode), as the core picks it (stream.rs) */
export function matchedSampleRate(codec: string, caps: CodecCaps | null, source: number): number {
  return nearestRate(
    offeredSampleRates(codec, caps).filter((r) => r > 0),
    source,
  );
}

/** The bit depth Auto streams `codec` with while it matches the PC's
 *  output (developer mode): 16-bit from a 16-bit output, else Auto's */
export function matchedBitDepth(codec: string, caps: CodecCaps | null, source: number): number {
  return source <= 16 && codecKey(codec) !== "aptxad" ? 16 : autoBitDepth(codec, caps);
}

/** The bit depths the dialog offers: the codec's, as far as the device
 *  lists them (LHDC V5 says) */
export function offeredBitDepths(codec: string, caps: CodecCaps | null): number[] {
  const listed = caps?.bit_depths ?? [];
  return bitDepths(codec).filter((b) => b === 0 || listed.length === 0 || listed.includes(b));
}

/**
 * The profile's ABR flag means AAC's VBR under AAC, the adaptive bitpool
 * under SBC and under SBC XQ (each its own: issue #86, SBC's turned XQ's on),
 * and LDAC's / LHDC V5's ABR elsewhere: switching between those leaves it
 * off rather than carrying it over (an LDAC profile with ABR moved to SBC
 * does not start adapting its bitpool unasked).
 */
export function abrCarries(from: string, to: string): boolean {
  const family = (c: string) => (c === "aac" || c === "sbc" || c === "sbcxq" ? c : "abr");
  return family(from) === family(to);
}

/**
 * A codec's starting values from another codec's (`p.codec`): what does
 * not carry over (ABR across families, SBC's frame setup to SBC XQ and
 * back) is left at the default, then the values are fitted to the codec
 */
export function carryToCodec<T extends CodecFields>(p: T, to: string): T {
  const same = p.codec === to;
  return fitToCodec({
    ...p,
    codec: to,
    abr: abrCarries(p.codec, to) ? p.abr : false,
    // Each codec's own: its card's "Show only supported options"
    ...(same || p.unlisted_options === undefined ? {} : { unlisted_options: false }),
    ...(same || p.sbc_blocks === undefined ? {} : { sbc_blocks: 0 }),
    ...(same || p.sbc_subbands === undefined ? {} : { sbc_subbands: 0 }),
    ...(same || p.sbc_allocation === undefined ? {} : { sbc_allocation: "" }),
  });
}

/** LHDC V5 low latency can be asked for (issue #543): the core asks only a
 *  device that lists it, so the switch is left out for one known not to */
export function lowLatencyOffered(codec: string, caps: CodecCaps | null): boolean {
  return codec === "lhdcv5" && (caps?.lhdcv5_low_latency ?? true);
}

/** LHDC V5 ABR runs at 400 kbps or below whatever the quality (as on
 *  Android): the quality choice is greyed out then */
export function qualityIgnored(codec: string, abr: boolean): boolean {
  return codec === "lhdcv5" && abr;
}

/** LDAC's rates at 44.1 / 88.2 kHz, LHDC V5's Standard at 44.1 kHz (88.2
 *  kHz runs as 96 kHz there): v1's profile list */
const RATE_44K: Partial<Record<CodecId, Record<string, string>>> = {
  ldac: { hq: "909", sq: "606", mq: "303" },
  lhdcv5: { sq: "480" },
};
/** aptX Adaptive at 96 kHz (an R2 device's 96 kHz levels; Low Latency runs
 *  at 48 kHz) */
const RATE_96K: Partial<Record<CodecId, Record<string, string>>> = {
  aptxad: { hq: "646" },
};

/** A quality's bit rate at the profile's sample rate (0 = auto), at most
 *  the device's largest (`maxBitrate` in bps, 0 = not known: AAC's, LHDC
 *  V5's; the core caps it the same way, issue #86) */
export function qualityKbps(codec: string, quality: string, sampleRate: number, maxBitrate = 0): string | undefined {
  const spec = codecSpec(codec);
  const q = spec.qualities.find((x) => x.id === quality) ?? spec.qualities[0];
  if (!q) return undefined;
  const is441 = codec === "lhdcv5" ? sampleRate === 44100 : sampleRate === 44100 || sampleRate === 88200;
  const table = is441 ? RATE_44K : sampleRate === 96000 ? RATE_96K : {};
  const kbps = table[spec.id]?.[q.id] ?? q.kbps;
  const cap = Math.floor(maxBitrate / 1000);
  return kbps && cap > 0 && /^\d+$/.test(kbps) && Number(kbps) > cap ? String(cap) : kbps;
}

/** "384 kbps": a codec's one bit rate at a sample rate of 44.1 or 48 kHz, for
 *  one without qualities (issue #86) */
export function fixedKbpsText(codec: string, sampleRate: number): string {
  const kbps = codecSpec(codec).fixedKbps;
  return kbps ? `${kbps[sampleRate === 44100 ? 0 : 1]} kbps` : "";
}

/**
 * The qualities the dialog offers for a codec on a device whose largest
 * bit rate is `maxBitrate` (bps, 0 = not known), each with its bit rate
 * there: one the device caps to the same bit rate as a higher one is left
 * out (LHDC V5 at most 400 kbps: High only), unless it is `current`
 */
export function offeredQualities(
  codec: string,
  sampleRate: number,
  maxBitrate = 0,
  current = "",
): { id: string; label: string; kbps?: string }[] {
  const out: { id: string; label: string; kbps?: string }[] = [];
  for (const q of codecSpec(codec).qualities) {
    // Low Latency is aptX Adaptive LL's card (issue #86)
    if (codec === "aptxad" && q.id === "ll") continue;
    const kbps = qualityKbps(codec, q.id, sampleRate, maxBitrate);
    if (q.id !== current && kbps && out.some((o) => o.kbps === kbps)) continue;
    out.push({ id: q.id, label: q.label, kbps });
  }
  return out;
}

/**
 * The quality of a profile as the profile list shows it (v1's
 * rebuild_profile_list): "High (990 kbps)", "ABR (≤400 kbps)" under LHDC V5
 * ABR; null when the codec has no quality choice (aptX family, FastStream,
 * LE Audio). `label` translates the quality's key.
 */
export function qualityTag(
  p: { codec: string; quality: string; abr: boolean; sample_rate: number; le_audio?: boolean },
  label: (key: string) => string,
): string | null {
  if (p.le_audio) return null;
  const spec = codecSpec(p.codec);
  if (spec.qualities.length === 0) return null;
  if (qualityIgnored(p.codec, p.abr)) return "ABR (≤400 kbps)";
  const q = spec.qualities.find((x) => x.id === p.quality) ?? spec.qualities[0];
  const kbps = qualityKbps(p.codec, q.id, p.sample_rate);
  return kbps ? `${label(q.label)} (${kbps} kbps)` : label(q.label);
}

/** Why a stream's bit rate is below the profile's, appended to the codec
 *  line: ABR (LHDC V5 stays at 400 kbps or below), or the device's maximum */
export function bitrateNoteKey(info: { abr: boolean; bitrate_capped: boolean }): string | null {
  if (info.abr) return "quality.codec_abr";
  if (info.bitrate_capped) return "quality.codec_capped";
  return null;
}

/** 0 = auto. Auto may pick LDAC / LHDC V5, so it offers 16 / 24 too (the
 *  core carries 24-bit only when the codec it picks has it) */
export function bitDepths(codec: string): number[] {
  const spec = codecSpec(codec);
  return spec.hiRes || spec.id === "auto" ? [0, 16, 24] : [0];
}

/** The bit depth a codec without a choice carries (aptX HD and aptX
 *  Adaptive 24, the others 16) */
export function fixedBitDepth(codec: string): number {
  return codecSpec(codec).bits ?? 16;
}

interface CodecFields {
  codec: string;
  quality: string;
  abr: boolean;
  sample_rate: number;
  bit_depth: number;
  sbc_bitpool?: number;
  sbcxq_bitpool?: number;
  sbc_min_bitpool?: number;
  sbcxq_min_bitpool?: number;
  sbc_blocks?: number;
  sbc_subbands?: number;
  sbc_allocation?: string;
  sbc_channel_mode?: string;
  unlisted_options?: boolean;
  lhdcv5_low_latency?: boolean;
}

/** Values the codec cannot use go back to the defaults (v1 did the same when the codec changed) */
export function fitToCodec<T extends CodecFields>(p: T): T {
  const spec = codecSpec(p.codec);
  const quality = spec.qualities.length === 0 || spec.qualities.some((m) => m.id === p.quality) ? p.quality : "hq";
  // The frame setup is SBC's and SBC XQ's; the channel mode SBC's only (XQ
  // is dual channel, and plain SBC has no dual channel: issue #86)
  const sbc = p.codec === "sbc" || p.codec === "sbcxq";
  return {
    ...p,
    quality,
    abr: spec.abr || spec.vbr ? p.abr : false,
    sample_rate: sampleRates(p.codec).includes(p.sample_rate) ? p.sample_rate : 0,
    bit_depth: bitDepths(p.codec).includes(p.bit_depth) ? p.bit_depth : 0,
    ...(p.sbc_bitpool === undefined ? {} : { sbc_bitpool: p.codec === "sbc" ? p.sbc_bitpool : 0 }),
    ...(p.sbcxq_bitpool === undefined ? {} : { sbcxq_bitpool: p.codec === "sbcxq" ? p.sbcxq_bitpool : 0 }),
    ...(p.sbc_min_bitpool === undefined ? {} : { sbc_min_bitpool: p.codec === "sbc" ? p.sbc_min_bitpool : 0 }),
    ...(p.sbcxq_min_bitpool === undefined ? {} : { sbcxq_min_bitpool: p.codec === "sbcxq" ? p.sbcxq_min_bitpool : 0 }),
    ...(p.sbc_blocks === undefined ? {} : { sbc_blocks: sbc ? p.sbc_blocks : 0 }),
    ...(p.sbc_subbands === undefined ? {} : { sbc_subbands: sbc ? p.sbc_subbands : 0 }),
    ...(p.sbc_allocation === undefined ? {} : { sbc_allocation: sbc ? p.sbc_allocation : "" }),
    ...(p.sbc_channel_mode === undefined ? {} : { sbc_channel_mode: p.codec === "sbc" && p.sbc_channel_mode !== "dual_channel" ? p.sbc_channel_mode : "" }),
    ...(p.lhdcv5_low_latency === undefined ? {} : { lhdcv5_low_latency: p.codec === "lhdcv5" && p.lhdcv5_low_latency }),
  };
}

// Max media packet size (crates/bt-host/src/media.rs, MEDIA_PAYLOAD_LIMIT_MIN / MAX / DEFAULT)
export const PACKET_MIN = 679;
export const PACKET_MAX = 1679;
export const PACKET_DEFAULT = 1023;
