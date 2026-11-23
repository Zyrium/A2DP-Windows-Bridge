//! The command line: `bluecaster-core --cli -d XX:XX:XX:XX:XX:XX [options]`.
//! The options are v1's; streaming goes through the same core service as the
//! resident process.
//!
//! Without -d (and without a receiver giving its address), the first
//! Windows-paired device whose class of device is Audio/Video is used, as v1
//! did; -l lists the devices Windows has paired. Both read Windows' device
//! list only: the adapter BlueCaster uses is not touched.
//!
//! `--le-audio` (implied by `--le-device` and `--le-scan`) streams LC3 over
//! LE Audio (experimental) instead: v1's le_audio_cli.cpp flow and printed
//! lines (the emulator tests read them), on the same core service with LE
//! Audio forced on.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// `println!` that survives a closed stdout (the console or pipe reading
/// the CLI went away: the run goes on, nothing is printed)
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}
/// `eprintln!` that survives a closed stderr
macro_rules! eoutln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}
/// `print!` that survives a closed stdout
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let mut o = std::io::stdout();
        let _ = write!(o, $($arg)*);
        let _ = o.flush();
    }};
}

use bluecaster_core::{
    HeadsetMic, Service, ServiceEvent, ServiceOptions, SourceRequest, StreamRequest, StreamState, VolumeSync,
};
use bt_host::HciCapture;
use bt_host::media::clamp_media_payload_limit;
use codec::{CodecKind, Quality};
use hci_transport::UsbSelector;
use win_sys::bt_paired::{self, PairedDevice};

const USAGE: &str = "\
Usage: bluecaster-core --cli [-d XX:XX:XX:XX:XX:XX] [options]

  -d <address>          Bluetooth address of the headphones / receiver
                        (default: the first audio device paired with Windows)
  --device-name <name>  its name, with -d (as a saved device's: Huawei earbuds
                        are told by it)
  -l                  list the Bluetooth devices paired with Windows and exit
  -u <adapter>          USB Bluetooth adapter to use when several have WinUSB:
                        instance ID (USB\\VID_0BDA&PID_8771\\...), device path
                        (\\\\?\\usb#vid_0bda&pid_8771#...) or VID:PID (0bda:8771)
  -c <codec>           ldac, aptxhd, aptxll, aptx, aptxad, sbcxq, sbc, aac, lhdcv5, opus, opus05, faststream, auto (default)
  -q <quality>          hq (default); sq, mq for LDAC and LHDC V5; mhq for LHDC V5; ll (low latency)
                        for aptX Adaptive (other codecs: hq, their highest bit rate)
  -a                    adaptive bit rate (LDAC, LHDC V5, aptX Adaptive; SBC / SBC XQ: bitpool)
  --lhdcv5-ll           LHDC V5 low latency, where the device lists it
  --fallback            a codec -c the device does not offer falls back to the
                        best one it offers (as the app does), instead of an error
  --force-codec         a codec -c the device does not offer is asked for anyway
  --unlisted-options    the codec's settings beyond what the device lists
  -m <mode>             capture: loopback (default), virtual, app
  --audio-device <id>   render device for -m virtual
  --app <exe>           executable for -m app
  --rate <hz>           capture sample rate (default: 48 kHz, or the best the device lists)
  --bits <16|24>        bit depth (default: 24 where the codec carries it)
  --max-packet <bytes>  max media packet size (679-1679, default 1023)
  --sbc-blocks <n>      SBC / SBC XQ block length: 4, 8, 12, 16 (default)
  --sbc-subbands <n>    SBC / SBC XQ subbands: 4, 8 (default)
  --sbc-alloc <method>  SBC / SBC XQ allocation: loudness (default), snr
  --sbc-mode <mode>     SBC channel mode: joint_stereo (default), stereo,
                        mono (dual channel: -c sbcxq)
  --sbc-bitpool <n>     SBC / SBC XQ bitpool (default: by quality)
  --afh <policy>        auto, off or Wi-Fi channels to avoid such as 6,11;
                        blend-mild, blend-medium, blend-strong
  --remote-sink <host[:port]|auto>
                        statistics of a measuring receiver (tools/linux_sink);
                        without -d, stream to that receiver
  --scan [seconds]      list nearby devices and exit (with --le-audio: the LE
                        Audio devices too)
  --probe               read what the adapter answers (read commands only, no
                        HCI init; for adapters BlueCaster cannot use yet), save it
                        to logs\\probe and exit
  --script <file>       run a chip script on the adapter in place of HCI init
                        (docs/chip-support.md), save its report and exit
  --allow-writes        let --script send requests that may change the chip

LE Audio (experimental, LC3 over LE isochronous channels):
  --le-audio            stream to the LE Audio device -d (BAP unicast, LC3)
  --le-device <address> another device of the group (e.g. the second earbud)
  --le-preset <name>    use only this BAP configuration (e.g. 48_2_2, 16_2_1)
  --le-low-latency      GMAP low latency (7.5 / 10 ms frames) for a gaming headset
  --le-scan [seconds]   list the LE Audio devices nearby and exit
  --qcy-probe [seconds] QCY earbuds (MeloBuds N70): scan, connect for GATT only (no
                        pairing) and read their state with read-only queries; -d <address>
                        picks the earbuds instead of the first N70. Changes nothing.
  --auracast <name>     broadcast under this name instead (Auracast source, no
                        device; --le-preset picks 16_2_2 ... 48_6_2, default
                        48_4_2)
  --auracast-code <code>
                        encrypt the broadcast with this code (4-16 bytes)
  --auracast-mono       one mono channel instead of left and right
  --auracast-layout <layout>
                        stereo (default); split: left and right as two
                        broadcasts of their own (<name> L / <name> R, a mono
                        channel each); matrix-<front>-<rear>: the matrix
                        mixer's outputs, front (FL FR) and rear (RL RR) each
                        stereo (one broadcast), separate (a mono broadcast
                        each), subgroups (one broadcast of two subgroups) or
                        off; older names: quad, front-rear, pairs,
                        rear-subgroups, subgroups
  --auracast-split      the same as --auracast-layout split
  --auracast-rear-preset <name>
                        use this BAP configuration for a matrix layout's rear
                        (e.g. 24_2_2 with a 48 kHz front; same frame duration)

Headset microphone:
  --headset-mic [on|auto]
                        headset mode: once streaming, the headphones'
                        microphone goes to --mic-output; classic devices switch
                        to the voice link (call quality), LE Audio devices use
                        the conversational configuration. auto: classic
                        devices switch only while an app records from the
                        output's recording side (a WAV file counts as always)
  --mic-output <name|path.wav>
                        the microphone's output device (a part of its name;
                        default: the default output), or a WAV file to write
  --voice-codec <cvsd|msbc|lc3_swb>
                        classic devices: the best call codec to use (one the
                        headphones lack falls back to the next lower one;
                        default: the best both sides have)
  --mic-rate <hz>       LE Audio: the microphone's sampling rate (16000,
                        24000, 32000, 48000; one the headphones do not allow
                        falls back; default: the first they allow)
  --mic-gain <db>       the microphone's gain (-12 to 24; default 0)
  --mic-noise-suppression
                        suppress noise on the microphone

Keyboards and mice (experimental):
  --hid-test <seconds>  start Bluetooth with keyboards and mice on and nothing
                        streamed, print their counts once a second for this
                        time and exit
  --hid-device <address>
                        a keyboard or mouse that may connect (repeatable); any
                        other is refused
  --hid-le-device <address>[/random]
                        an LE keyboard or mouse (repeatable), with a public
                        address or a random one; it is connected whenever it
                        advertises
  --hid-connect <address>
                        connect to this one (one of --hid-device or
                        --hid-le-device) and pair it as the app adds one;
                        without it the test waits for a device to connect.
                        Over --hci-tcp a passkey to type is printed as
                        'hid passkey shown <digits>'
  --hid-scan <seconds>  list nearby devices with keyboards and mice on (LE ones
                        too) and exit
  --hid-inject          pass the input to Windows; without it the input is only
                        decoded and counted. Never with --hci-tcp: the
                        emulator's input is always only counted
  --hid-leds <mask>     without --hid-inject: the keyboard lights to send (bit
                        0 Num Lock, 1 Caps Lock, 2 Scroll Lock) instead of
                        none
  --hid-confirm <seconds>
                        with --hid-connect: keep the device only if its input
                        arrives within this many seconds after it is ready
                        (as the app's 30 s check); otherwise it is removed.
                        Prints 'hid check waiting', then 'hid check
                        confirmed' or 'hid check removed <reason>'

Development / testing:
  --hci-tcp <host:port> H4 over TCP to a virtual controller (tools/emu)
  --hci-capture <path>  HCI capture (.pklg) of the whole run
  --test-tone           1 kHz left / 1.5 kHz right at 48 kHz instead of capture
  --session-agent       do the session's work (capture, the test tone, device
                        lists) in a second process, the session agent
                        (--capture-agent: the same)
  --duration <seconds>  stop after this time (default: Ctrl+C)
  --sessions <n>        stream n times in a row with --duration each, as
                        Disconnect / Connect in the app (A2DP)
  --wait-off            headphones turned off: wait for them, as the app does
  --services            once streaming, list the services the device gives in SDP
  --ipc                 serve the IPC pipe during the run (needs
                        BLUECASTER_INSTANCE, so it never takes the installed
                        core's pipe)
  --eq <point>[,<point>...]
                        the playback EQ on, a point as <Hz>:<dB>[:<Q>[:<type>]]
                        (type peak, lowshelf, highshelf, highpass, lowpass;
                        Q 1 and peak when left out)
  --headset-mic-toggle <on_s>,<off_s>
                        headset mode on <on_s> seconds after streaming starts
                        and off at <off_s> (the voice link and back to A2DP)
  --restart-on-link     start the session again as soon as its link is up, as
                        a setting changed while connecting does
  --restart             with --sessions: between sessions restart Bluetooth
                        as the Adapters page does, which connects again
  --le-sim-volume <level>[,<s>:<level>...]
                        LE Audio: VolumeSync with a simulated Windows volume
                        0-127 (changed at <s> seconds); the real volume is not
                        touched
  --mcp-dry-run         LE Audio: media control answered by a simulated player,
                        commands only logged (implied by --hci-tcp)
  --ccp-sim \"<s>:<command>;...\"
                        LE Audio: simulated calls (incoming, alerting,
                        answered, hold, retrieve, hangup)
  --ccp-console         LE Audio: call simulation commands from the console
";

struct Args {
    device: Option<String>,
    device_name: String,
    /// -l: list the Windows-paired devices and exit
    list: bool,
    /// -u: the adapter, as given and as parsed
    usb: Option<(String, UsbSelector)>,
    codec: Option<CodecKind>,
    quality: Quality,
    abr: bool,
    /// --lhdcv5-ll: LHDC V5 low latency (issue #543)
    lhdcv5_low_latency: bool,
    /// --fallback: a codec the device does not offer falls back, as a profile's
    fallback: bool,
    /// --force-codec: a codec the device does not offer is asked for anyway
    force_codec: bool,
    /// --unlisted-options: settings beyond what the device lists
    unlisted_options: bool,
    mode: String,
    audio_device: String,
    app: String,
    rate: Option<u32>,
    bits: Option<u8>,
    max_packet: u16,
    /// --sbc-*: the SBC setup and bitpool (defaults: 0 / None)
    sbc: codec::sbc::SbcParams,
    sbc_bitpool: u8,
    afh: Option<String>,
    remote_sink: Option<String>,
    scan: Option<u32>,
    probe: bool,
    /// --services: once streaming, the services the device lists in SDP
    services: bool,
    /// --ipc: the IPC pipe served during the run (a test instance's only)
    ipc: bool,
    /// --headset-mic [on|auto], --mic-output: headset mode once streaming
    headset_mic: HeadsetMic,
    mic_output: Option<String>,
    /// --voice-codec: the best call codec (None: Auto)
    voice_codec: Option<bluecaster_core::VoiceCodec>,
    /// --mic-rate: the LE Audio microphone's rate (None: Auto)
    mic_rate: Option<u32>,
    /// --eq: the playback EQ's points (empty: off)
    eq: Vec<dsp::EqBand>,
    /// --mic-gain, --mic-noise-suppression: the microphone's processing
    mic_processing: ipc::MicProcessing,
    /// --headset-mic-toggle: headset mode on and off at these seconds
    headset_mic_toggle: Option<(u64, u64)>,
    script: Option<PathBuf>,
    allow_writes: bool,
    hci_tcp: Option<String>,
    hci_capture: Option<PathBuf>,
    test_tone: bool,
    /// --session-agent (or --capture-agent): the session-bound work runs in
    /// the session agent (agent.rs)
    session_agent: bool,
    duration: Option<u64>,
    /// --sessions: streams one after the other in this process (Connect,
    /// --duration, Disconnect), as the UI's Disconnect / Connect (issue #84)
    sessions: u32,
    /// --restart: between sessions Bluetooth is restarted instead, as on
    /// the Adapters page, and the session connects again (issue #86)
    restart: bool,
    /// --restart-on-link: the first session starts again once its link is up
    restart_on_link: bool,
    /// --wait-off: headphones turned off do not end the run
    wait_off: bool,
    le: LeArgs,
    hid: HidArgs,
}

/// Keyboards and mice (experimental, issue #413) options
#[derive(Debug, Default, PartialEq, Eq)]
struct HidArgs {
    /// --hid-test <seconds>
    test: Option<u64>,
    /// --hid-device: the allow-list, as typed
    devices: Vec<String>,
    /// --hid-le-device: the LE part of it, as typed
    le_devices: Vec<String>,
    /// --hid-scan <seconds>
    scan: Option<u32>,
    /// --hid-connect: BlueCaster opens the connection
    connect: Option<String>,
    /// --hid-inject: the input goes to Windows (never over --hci-tcp)
    inject: bool,
    /// --hid-leds: the keyboard lights sent in dry run (the emulator test)
    leds: Option<u8>,
    /// --hid-helper-record: test switch of debug builds, the input goes
    /// through a real input helper that records it instead of injecting
    helper_record: bool,
    /// --hid-confirm: the countdown for --hid-connect's device
    confirm: Option<u64>,
}

/// LE Audio (experimental) options (v1 LeAudioCliOptions)
#[derive(Debug, Default, PartialEq, Eq)]
struct LeArgs {
    /// --le-audio, or implied by --le-device / --le-scan
    on: bool,
    /// --le-device: more devices of the group (after -d)
    devices: Vec<String>,
    preset: Option<String>,
    sim_volume: Option<String>,
    low_latency: bool,
    /// --le-scan [seconds]
    scan: Option<u32>,
    /// --qcy-probe [seconds]
    qcy_probe: Option<u32>,
    mcp_dry_run: bool,
    ccp_sim: Option<String>,
    ccp_console: bool,
    /// --auracast <name>: broadcast instead of a unicast stream
    broadcast: Option<String>,
    /// --auracast-code
    broadcast_code: Option<String>,
    /// --auracast-mono
    broadcast_mono: bool,
    /// --auracast-layout (--auracast-split: split)
    broadcast_layout: Option<String>,
    /// --auracast-rear-preset
    broadcast_rear_preset: Option<String>,
}

/// --le-scan without a time (v1 scan_seconds)
const LE_SCAN_SECONDS: u32 = 8;
/// --qcy-probe without a time
const QCY_PROBE_SECONDS: u32 = 10;

impl LeArgs {
    /// The stream request: --le-preset tries only that preset. GMAP low
    /// latency is the flag only (not the GUI setting), and an explicit
    /// preset wins: the message to print then.
    fn request(&self) -> (bt_host::LeStreamRequest, bool, Option<&'static str>) {
        let mut request = bt_host::LeStreamRequest::default();
        if let Some(p) = &self.preset {
            request.preset_order = vec![p.clone()];
        }
        let low_latency = self.low_latency && self.preset.is_none();
        let note =
            (self.low_latency && self.preset.is_some()).then_some("--le-preset given: --le-low-latency is not used");
        (request, low_latency, note)
    }
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args {
        device: None,
        device_name: String::new(),
        list: false,
        usb: None,
        codec: None,
        quality: Quality::High,
        abr: false,
        lhdcv5_low_latency: false,
        fallback: false,
        force_codec: false,
        unlisted_options: false,
        mode: "loopback".into(),
        audio_device: String::new(),
        app: String::new(),
        rate: None,
        bits: None,
        max_packet: bt_host::media::MEDIA_PAYLOAD_LIMIT_DEFAULT,
        sbc: codec::sbc::SbcParams::default(),
        sbc_bitpool: 0,
        afh: None,
        remote_sink: None,
        scan: None,
        probe: false,
        services: false,
        ipc: false,
        headset_mic: HeadsetMic::Off,
        mic_output: None,
        voice_codec: None,
        mic_rate: None,
        mic_processing: ipc::MicProcessing::default(),
        headset_mic_toggle: None,
        script: None,
        allow_writes: false,
        hci_tcp: None,
        hci_capture: None,
        test_tone: false,
        session_agent: false,
        eq: Vec::new(),
        duration: None,
        sessions: 1,
        restart: false,
        restart_on_link: false,
        wait_off: false,
        le: LeArgs::default(),
        hid: HidArgs::default(),
    };
    let mut it = args.iter().peekable();
    let value = |it: &mut std::iter::Peekable<std::slice::Iter<String>>, name: &str| {
        it.next().cloned().ok_or_else(|| format!("{name} needs a value"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--cli" => {}
            "-d" => a.device = Some(value(&mut it, "-d")?),
            "--device-name" => a.device_name = value(&mut it, "--device-name")?,
            "-l" => a.list = true,
            "-u" => {
                let u = value(&mut it, "-u")?;
                let selector = UsbSelector::from_arg(&u)?;
                a.usb = Some((u.trim().to_string(), selector));
            }
            "-c" => {
                let c = value(&mut it, "-c")?;
                a.codec = if c.eq_ignore_ascii_case("auto") {
                    None
                } else {
                    Some(CodecKind::from_key(&c).ok_or_else(|| {
                        format!("Unknown codec '{c}'. Use: ldac, aptxhd, aptxll, aptx, aptxad, sbcxq, sbc, aac, lhdcv5, opus, opus05, faststream, auto")
                    })?)
                };
            }
            "-q" => a.quality = Quality::from_key(&value(&mut it, "-q")?.to_lowercase()),
            "-a" => a.abr = true,
            "--lhdcv5-ll" => a.lhdcv5_low_latency = true,
            "--fallback" => a.fallback = true,
            "--force-codec" => a.force_codec = true,
            "--unlisted-options" => a.unlisted_options = true,
            "-m" => a.mode = value(&mut it, "-m")?.to_lowercase(),
            "--audio-device" => a.audio_device = value(&mut it, "--audio-device")?,
            "--app" => a.app = value(&mut it, "--app")?,
            "--rate" => a.rate = Some(value(&mut it, "--rate")?.parse().map_err(|_| "bad --rate")?),
            "--bits" => a.bits = Some(value(&mut it, "--bits")?.parse().map_err(|_| "bad --bits")?),
            "--max-packet" => {
                let v: u32 = value(&mut it, "--max-packet")?.parse().map_err(|_| "bad --max-packet")?;
                a.max_packet = clamp_media_payload_limit(v);
                if a.max_packet as u32 != v {
                    outln!("--max-packet {v} is outside 679-1679, using {}", a.max_packet);
                }
            }
            "--sbc-blocks" => {
                a.sbc.blocks = value(&mut it, "--sbc-blocks")?.parse().map_err(|_| "bad --sbc-blocks")?;
                if !matches!(a.sbc.blocks, 4 | 8 | 12 | 16) {
                    return Err("--sbc-blocks takes 4, 8, 12 or 16".into());
                }
            }
            "--sbc-subbands" => {
                a.sbc.subbands = value(&mut it, "--sbc-subbands")?.parse().map_err(|_| "bad --sbc-subbands")?;
                if !matches!(a.sbc.subbands, 4 | 8) {
                    return Err("--sbc-subbands takes 4 or 8".into());
                }
            }
            "--sbc-alloc" => {
                let v = value(&mut it, "--sbc-alloc")?.to_lowercase();
                a.sbc.allocation =
                    Some(codec::sbc::Allocation::from_key(&v).ok_or("--sbc-alloc takes loudness or snr")?);
            }
            "--sbc-mode" => {
                let v = value(&mut it, "--sbc-mode")?.to_lowercase();
                // Dual channel is SBC XQ's (issue #86)
                a.sbc.channel_mode = Some(
                    codec::sbc::ChannelMode::from_key(&v)
                        .filter(|m| *m != codec::sbc::ChannelMode::DualChannel)
                        .ok_or("--sbc-mode takes joint_stereo, stereo or mono (dual channel: -c sbcxq)")?,
                );
            }
            "--sbc-bitpool" => {
                a.sbc_bitpool = value(&mut it, "--sbc-bitpool")?.parse().map_err(|_| "bad --sbc-bitpool")?;
                if !(2..=53).contains(&a.sbc_bitpool) {
                    return Err("--sbc-bitpool takes 2 to 53".into());
                }
            }
            "--afh" => a.afh = Some(value(&mut it, "--afh")?),
            "--remote-sink" => a.remote_sink = Some(value(&mut it, "--remote-sink")?),
            "--scan" => {
                let secs = match it.peek() {
                    Some(s) if !s.starts_with('-') => it.next().and_then(|s| s.parse().ok()).unwrap_or(10),
                    _ => 10,
                };
                a.scan = Some(secs);
            }
            "--probe" => a.probe = true,
            "--services" => a.services = true,
            "--ipc" => a.ipc = true,
            "--headset-mic" => {
                // An optional mode; alone: on
                let mode = match it.peek().map(|s| s.as_str()) {
                    Some("on") => Some(HeadsetMic::On),
                    Some("auto") => Some(HeadsetMic::Auto),
                    _ => None,
                };
                if mode.is_some() {
                    it.next();
                }
                a.headset_mic = mode.unwrap_or(HeadsetMic::On);
            }
            "--mic-output" => a.mic_output = Some(value(&mut it, "--mic-output")?),
            "--voice-codec" => {
                let v = value(&mut it, "--voice-codec")?;
                a.voice_codec = Some(
                    bluecaster_core::VoiceCodec::from_name(&v).ok_or("--voice-codec takes cvsd, msbc or lc3_swb")?,
                );
            }
            "--mic-gain" => {
                let v = value(&mut it, "--mic-gain")?;
                a.mic_processing.gain_db = v
                    .parse::<i8>()
                    .ok()
                    .filter(|db| (-12..=24).contains(db))
                    .ok_or("--mic-gain takes -12 to 24 (dB)")?;
                a.mic_processing.enabled = true;
            }
            "--mic-noise-suppression" => {
                a.mic_processing.suppression = 100;
                a.mic_processing.enabled = true;
            }
            "--mic-rate" => {
                let v = value(&mut it, "--mic-rate")?;
                a.mic_rate = Some(v.parse::<u32>().ok().filter(|&hz| hz > 0).ok_or("--mic-rate takes a rate in Hz")?);
            }
            "--eq" => a.eq = parse_eq(&value(&mut it, "--eq")?)?,
            "--headset-mic-toggle" => {
                let v = value(&mut it, "--headset-mic-toggle")?;
                let (on, off) = v
                    .split_once(',')
                    .and_then(|(on, off)| Some((on.trim().parse::<u64>().ok()?, off.trim().parse::<u64>().ok()?)))
                    .filter(|(on, off)| on < off)
                    .ok_or("--headset-mic-toggle takes <on_s>,<off_s> with on before off")?;
                a.headset_mic_toggle = Some((on, off));
            }
            "--script" => a.script = Some(value(&mut it, "--script")?.into()),
            "--allow-writes" => a.allow_writes = true,
            "--hci-tcp" => a.hci_tcp = Some(value(&mut it, "--hci-tcp")?),
            "--hci-capture" => a.hci_capture = Some(value(&mut it, "--hci-capture")?.into()),
            "--test-tone" => a.test_tone = true,
            "--session-agent" | "--capture-agent" => a.session_agent = true,
            "--duration" => a.duration = Some(value(&mut it, "--duration")?.parse().map_err(|_| "bad --duration")?),
            "--sessions" => {
                a.sessions = value(&mut it, "--sessions")?.parse().map_err(|_| "bad --sessions")?;
                if a.sessions == 0 {
                    return Err("--sessions needs at least 1".into());
                }
            }
            "--restart" => a.restart = true,
            "--restart-on-link" => a.restart_on_link = true,
            "--wait-off" => a.wait_off = true,
            "--le-audio" => a.le.on = true,
            "--le-device" => {
                a.le.on = true;
                a.le.devices.push(value(&mut it, "--le-device")?);
            }
            "--le-preset" => a.le.preset = Some(value(&mut it, "--le-preset")?),
            "--le-sim-volume" => a.le.sim_volume = Some(value(&mut it, "--le-sim-volume")?),
            "--le-low-latency" => a.le.low_latency = true,
            "--le-scan" => {
                a.le.on = true;
                let secs = match it.peek() {
                    Some(s) if !s.starts_with('-') => it.next().and_then(|s| s.parse().ok()).unwrap_or(LE_SCAN_SECONDS),
                    _ => LE_SCAN_SECONDS,
                };
                a.le.scan = Some(secs);
            }
            "--qcy-probe" => {
                a.le.on = true;
                let secs = match it.peek() {
                    Some(s) if !s.starts_with('-') => {
                        it.next().and_then(|s| s.parse().ok()).unwrap_or(QCY_PROBE_SECONDS)
                    }
                    _ => QCY_PROBE_SECONDS,
                };
                a.le.qcy_probe = Some(secs);
            }
            "--mcp-dry-run" => a.le.mcp_dry_run = true,
            "--ccp-sim" => a.le.ccp_sim = Some(value(&mut it, "--ccp-sim")?),
            "--ccp-console" => a.le.ccp_console = true,
            "--auracast" => {
                a.le.on = true;
                a.le.broadcast = Some(value(&mut it, "--auracast")?);
            }
            "--auracast-code" => a.le.broadcast_code = Some(value(&mut it, "--auracast-code")?),
            "--auracast-mono" => a.le.broadcast_mono = true,
            "--hid-test" => {
                let v = value(&mut it, "--hid-test")?;
                a.hid.test = Some(v.parse().ok().filter(|&s| s > 0).ok_or("--hid-test takes seconds (1 or more)")?);
            }
            "--hid-device" => a.hid.devices.push(value(&mut it, "--hid-device")?),
            "--hid-le-device" => a.hid.le_devices.push(value(&mut it, "--hid-le-device")?),
            "--hid-scan" => {
                let v = value(&mut it, "--hid-scan")?;
                a.hid.scan = Some(v.parse().ok().filter(|&s| s > 0).ok_or("--hid-scan takes seconds (1 or more)")?);
            }
            "--hid-connect" => a.hid.connect = Some(value(&mut it, "--hid-connect")?),
            "--hid-inject" => a.hid.inject = true,
            "--hid-helper-record" => a.hid.helper_record = true,
            "--hid-confirm" => {
                let v = value(&mut it, "--hid-confirm")?;
                a.hid.confirm = Some(
                    v.parse().ok().filter(|&s| (1..=600).contains(&s)).ok_or("--hid-confirm takes 1 to 600 seconds")?,
                );
            }
            "--hid-leds" => {
                let v = value(&mut it, "--hid-leds")?;
                a.hid.leds = Some(v.parse().ok().filter(|&m| m <= 7).ok_or("--hid-leds takes a mask from 0 to 7")?);
            }
            "--auracast-split" => a.le.broadcast_layout = Some("split".into()),
            "--auracast-layout" => {
                let v = value(&mut it, "--auracast-layout")?;
                if bt_host::BroadcastLayout::parse(&v).is_none() {
                    return Err(
                        "--auracast-layout takes stereo, mono, split, matrix-<front>-<rear> (each stereo, separate, subgroups or off), quad, front-rear, pairs, rear-subgroups or subgroups"
                            .into(),
                    );
                }
                a.le.broadcast_layout = Some(v);
            }
            "--auracast-rear-preset" => {
                a.le.broadcast_rear_preset = Some(value(&mut it, "--auracast-rear-preset")?);
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    // Mono: one channel, not split or a matrix
    let layout = a.le.broadcast_layout.as_deref().and_then(bt_host::BroadcastLayout::parse);
    if a.le.broadcast_mono {
        match layout {
            Some(bt_host::BroadcastLayout::Split) => {
                return Err("--auracast-mono and --auracast-split cannot be used together".into());
            }
            Some(l) if l.is_matrix() => {
                return Err(format!("--auracast-mono and --auracast-layout {} cannot be used together", l.as_str()));
            }
            _ => {}
        }
    }
    let hid_options =
        !a.hid.devices.is_empty() || !a.hid.le_devices.is_empty() || a.hid.connect.is_some() || a.hid.inject;
    if a.hid.helper_record && (a.hid.test.is_none() || a.hid.inject) {
        return Err("--hid-helper-record needs --hid-test and cannot be used with --hid-inject".into());
    }
    if a.hid.test.is_none() && (hid_options || a.hid.leds.is_some()) {
        return Err("--hid-device, --hid-le-device, --hid-connect, --hid-inject and --hid-leds need --hid-test".into());
    }
    if a.hid.confirm.is_some() && a.hid.connect.is_none() {
        return Err("--hid-confirm needs --hid-connect".into());
    }
    if a.hid.test.is_some() && a.hid.scan.is_some() {
        return Err("--hid-test and --hid-scan cannot be used together".into());
    }
    for d in a.hid.devices.iter().chain(&a.hid.connect) {
        if parse_device_address(d).is_none() {
            return Err(format!("Invalid Bluetooth address: {d}"));
        }
    }
    for d in &a.hid.le_devices {
        if parse_hid_le_address(d).is_none() {
            return Err(format!("Invalid LE address: {d} (an address, with /random for a random one)"));
        }
    }
    if let Some(c) = &a.hid.connect {
        let listed = a.hid.devices.iter().any(|d| parse_device_address(d) == parse_device_address(c))
            || a.hid.le_devices.iter().any(|d| parse_hid_le_address(d).map(|l| l.addr) == parse_device_address(c));
        if !listed {
            return Err("--hid-connect needs the same address as a --hid-device or --hid-le-device".into());
        }
    }
    Ok(a)
}

pub fn run(args: &[String]) -> i32 {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eoutln!("{e}\n");
            }
            out!("{USAGE}");
            return if e.is_empty() { 0 } else { 2 };
        }
    };
    if a.list {
        // Windows' list only: no service, no adapter
        return match bt_paired::paired_devices() {
            Ok(devices) => {
                print_paired(&devices);
                outln!("\nDone.");
                0
            }
            Err(e) => {
                eoutln!("BluetoothFindFirstDevice failed: {e}");
                1
            }
        };
    }
    if a.session_agent
        && let Err(e) = crate::agent::use_agent()
    {
        eoutln!("{e}");
        return 1;
    }
    let running = Arc::new(AtomicBool::new(true));
    {
        let running = running.clone();
        let _ = ctrlc::set_handler(move || running.store(false, Ordering::Release));
    }

    match (&a.hci_tcp, &a.usb) {
        (Some(addr), usb) => {
            outln!("Transport: H4 over TCP to {addr} (development / testing)");
            if let Some((u, _)) = usb {
                outln!("-u {u} is not used with --hci-tcp");
            }
        }
        (None, Some((u, _))) => outln!("Transport: USB (WinUSB), adapter {u}"),
        (None, None) => outln!("Transport: USB (WinUSB)"),
    }
    let capture = HciCapture::new();
    if let Some(path) = &a.hci_capture
        && let Err(e) = capture.start(path)
    {
        eoutln!("Cannot create HCI capture file {}: {e}", path.display());
        return 1;
    }
    if a.ipc && ipc::instance_suffix().is_empty() {
        eoutln!("--ipc needs BLUECASTER_INSTANCE (a test instance's own pipe)");
        return 2;
    }
    let (tx, rx) = mpsc::channel();
    let service = Arc::new(Service::new(
        ServiceOptions {
            config_dir: bluecaster_core::paths::config_dir(),
            hci_tcp: a.hci_tcp.clone(),
            capture: capture.clone(),
            afh: a.afh.clone(),
            usb: a.usb.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
        },
        tx,
    ));
    if a.ipc {
        serve_ipc(service.clone());
    }
    if !a.eq.is_empty() {
        let eq = bluecaster_core::media_eq::MediaEq { enabled: true, bands: a.eq.clone() };
        outln!("Playback EQ: {} point(s)", eq.settings().bands().len());
        bluecaster_core::media_eq::apply(Some(&eq));
    }
    // --headset-mic-toggle: the stack offers HFP audio from the start, as
    // a stream started in headset mode would
    if a.headset_mic_toggle.is_some() {
        service.set_voice_offered(true);
    }

    if let Some(secs) = a.hid.test {
        let code = hid_test(&a, &service, secs, &running);
        service.shutdown();
        capture.stop();
        return code;
    }

    if let Some(secs) = a.hid.scan {
        // The app's device scan with keyboards and mice on: LE ones are
        // listed too (no device allowed, nothing connects)
        service.set_hid_cli(Vec::new(), Vec::new(), true);
        let code = match service.scan_devices(secs, &mut |_| {}) {
            Ok(found) => {
                print_scanned(&found);
                0
            }
            Err(m) => {
                eoutln!("{m}");
                1
            }
        };
        service.shutdown();
        capture.stop();
        return code;
    }

    if a.le.broadcast.is_some() {
        let code = le_broadcast(&a, &service, &rx, &running);
        service.shutdown();
        capture.stop();
        return code;
    }

    if let (Some(secs), true) = (a.scan, a.le.on) {
        // The app's device scan: the classic devices, and the LE Audio ones
        // of an LE scan beside it
        service.set_le_audio_cli(bluecaster_core::LeCliOptions { mcp_dry_run: a.le.mcp_dry_run, ..Default::default() });
        let code = match service.scan_devices(secs, &mut |_| {}) {
            Ok(found) => {
                print_scanned(&found);
                0
            }
            Err(m) => {
                eoutln!("{m}");
                1
            }
        };
        service.shutdown();
        capture.stop();
        return code;
    }

    if a.le.on {
        // LE Audio (experimental): its own flow (v1 le_audio_cli.cpp)
        let code = le_run(&a, &service, &running);
        service.shutdown();
        capture.stop();
        return code;
    }

    if let Some(file) = &a.script {
        let code = run_script(&service, file, a.allow_writes, a.usb.as_ref().map(|(u, _)| u.as_str()));
        service.shutdown();
        capture.stop();
        return code;
    }

    if a.probe {
        let code = match service.probe_adapter(None) {
            Ok(report) => {
                for s in &report.steps {
                    let answer = match (&s.answer, &s.error) {
                        (Some(a), _) => a.iter().map(|b| format!("{b:02x}")).collect(),
                        (None, Some(e)) => format!("failed: {e}"),
                        (None, None) => "no answer".to_owned(),
                    };
                    outln!("{:<34} {:>5} ms  {answer}", s.name, s.millis);
                }
                if let Some(usb) = &report.usb {
                    outln!();
                    for line in &usb.summary {
                        outln!("{line}");
                    }
                }
                outln!("\n{}", bluecaster_core::probe_report::summary_line(&report));
                let adapter = a.usb.as_ref().map(|(u, _)| u.as_str());
                match bluecaster_core::probe_report::save(
                    &bluecaster_core::paths::config_dir(),
                    &report,
                    adapter,
                    env!("CARGO_PKG_VERSION"),
                ) {
                    Ok(path) => {
                        outln!("Saved to {}", path.display());
                        0
                    }
                    Err(e) => {
                        eoutln!("The report could not be saved: {e}");
                        1
                    }
                }
            }
            Err(m) => {
                eoutln!("{m}");
                1
            }
        };
        service.shutdown();
        capture.stop();
        return code;
    }

    if let Some(secs) = a.scan {
        let code = match service.scan(secs) {
            Ok(found) => {
                for d in found {
                    outln!(
                        "{}  {:<32} CoD 0x{:06X}  RSSI {}{}",
                        bt_host::addr_str(&d.addr),
                        d.name,
                        d.class_of_device,
                        d.rssi,
                        if d.is_audio() { "  (audio)" } else { "" }
                    );
                }
                0
            }
            Err(m) => {
                eoutln!("{m}");
                1
            }
        };
        service.shutdown();
        capture.stop();
        return code;
    }

    // The measuring receiver: its IP for the statistics and, without -d, its
    // Bluetooth address as the device to stream to
    let receiver = bluecaster_core::receiver::ReceiverClient::new();
    let mut device_arg = a.device.clone();
    if let Some(target) = &a.remote_sink {
        use bluecaster_core::receiver;
        let Some((host, port)) = receiver::split_target(target) else {
            eoutln!("Bad --remote-sink {target}");
            return 2;
        };
        let auto = host.eq_ignore_ascii_case(receiver::AUTO);
        let found = receiver::discover((!auto).then_some(host.as_str()), port, receiver::DISCOVER_TIMEOUT);
        let mut stats_target = target.clone();
        match found.first() {
            None => outln!("Receiver: none answered {}", if auto { "on the local network" } else { host.as_str() }),
            Some(r) => {
                outln!("{}", r.describe(found.len() > 1));
                if device_arg.is_none() && !r.bt_address.is_empty() {
                    device_arg = Some(r.bt_address.clone());
                    outln!("Streaming to the receiver (no -d given)");
                }
                if auto {
                    stats_target = format!("{}:{}", r.ip, r.port);
                }
            }
        }
        outln!("Receiver statistics: {stats_target}");
        receiver.start(&stats_target);
    }

    let stop = |code: i32| {
        service.shutdown();
        capture.stop();
        if a.remote_sink.is_some() {
            receiver.stop();
        }
        code
    };
    let mut device_name = a.device_name.clone();
    let device = match device_arg.as_deref() {
        Some(d) => match parse_device_address(d) {
            Some(addr) => addr,
            None => {
                eoutln!("Invalid Bluetooth address: {d}");
                return stop(1);
            }
        },
        // v1: the first Windows-paired device that is an A2DP sink by its
        // class of device (works while the adapter has WinUSB: Windows
        // remembers its paired devices)
        None => {
            outln!("Scanning for Bluetooth audio devices...");
            let devices = match bt_paired::paired_devices() {
                Ok(devices) => devices,
                Err(e) => {
                    eoutln!("BluetoothFindFirstDevice failed: {e}");
                    Vec::new()
                }
            };
            print_paired(&devices);
            match bt_paired::first_a2dp_sink(&devices) {
                Some(d) => {
                    outln!("Auto-selected device: {} ({})", d.name, d.address_string());
                    device_name = d.name.clone();
                    d.address
                }
                None => {
                    eoutln!(
                        "No Bluetooth audio device found.\n\
                         Use -d XX:XX:XX:XX:XX:XX to specify the device address.\n\
                         (Find the address in Windows Settings > Bluetooth before switching to WinUSB,\n\
                         or list the devices nearby with --scan)"
                    );
                    return stop(1);
                }
            }
        }
    };
    let source = if a.test_tone {
        SourceRequest::TestTone
    } else {
        match a.mode.as_str() {
            "virtual" => SourceRequest::VirtualDevice {
                id: a.audio_device.clone(),
                name: a.audio_device.clone(),
                auto_switch: true,
            },
            "app" => SourceRequest::Application { exe: a.app.clone() },
            _ => SourceRequest::SystemLoopback,
        }
    };
    outln!("Target device: {}", bt_host::addr_str(&device));
    let request = StreamRequest {
        device,
        device_name,
        codec: a.codec,
        codec_fallback: a.fallback,
        codec_forced: a.force_codec,
        unlisted_options: a.unlisted_options,
        // The command line is a developer's: every codec, as asked
        developer_codecs: true,
        auto_order: codec::AutoOrder::default(),
        quality: a.quality,
        abr: a.abr,
        lhdcv5_low_latency: a.lhdcv5_low_latency,
        sbc_bitpool: a.sbc_bitpool,
        sbcxq_bitpool: a.sbc_bitpool,
        sbc_min_bitpool: 0,
        sbcxq_min_bitpool: 0,
        sbc: a.sbc,
        sample_rate: a.rate,
        bit_depth: a.bits,
        codec_values: Vec::new(),
        source,
        volume_sync: VolumeSync::Off,
        mute_output: false,
        max_media_payload: a.max_packet,
        le: None,
        headset_mic: if a.headset_mic_toggle.is_none() { a.headset_mic } else { HeadsetMic::Off },
        mic_output: a.mic_output.clone(),
        voice_codec: a.voice_codec,
        mic_processing: a.mic_processing,
    };

    let mut code = 0;
    let mut request = request;
    for session in 1..=a.sessions {
        if session > 1 && a.restart {
            // Restart Bluetooth, as the Adapters page does: the session
            // connects again on the new stack
            outln!("\nRestarting Bluetooth (session {} of {})...", session - 1, a.sessions);
            let Some(again) = service.restart_adapter() else {
                outln!("Bluetooth restarted, but nothing to connect again");
                code = 1;
                break;
            };
            request = again;
            std::thread::sleep(SESSION_PAUSE);
            if !running.load(Ordering::Acquire) {
                break;
            }
            outln!("Connecting again after the restart (session {session} of {})", a.sessions);
        } else if session > 1 {
            // Disconnect, as the app's button does, and connect again
            outln!("\nDisconnecting (session {} of {})...", session - 1, a.sessions);
            service.disconnect();
            while let Ok(e) = rx.try_recv() {
                if let ServiceEvent::State(state, msg) = e {
                    outln!("[{state:?}] {msg}");
                }
            }
            std::thread::sleep(SESSION_PAUSE);
            if !running.load(Ordering::Acquire) {
                break;
            }
            outln!("Connecting again (session {session} of {})", a.sessions);
        }
        service.start_streaming(request.clone());
        if a.restart_on_link && session == 1 {
            let until = Instant::now() + Duration::from_secs(10);
            while Instant::now() < until && !service.host().is_some_and(|h| h.link_age(device).is_some()) {
                std::thread::sleep(Duration::from_millis(5));
            }
            outln!("Starting again while connecting");
            service.start_streaming(request.clone());
        }
        let duration_reached = stream_session(&a, &service, &rx, &running, &receiver, &mut code);
        if !duration_reached || code != 0 {
            break;
        }
    }
    service.shutdown();
    capture.stop();
    if a.remote_sink.is_some() {
        // Let the last packets arrive and the receiver report them
        std::thread::sleep(Duration::from_millis(1500));
    }
    let stats = service.stats();
    outln!(
        "Sent {} media packets ({} bytes), {} dropped before the radio",
        stats.packets_sent.load(Ordering::Relaxed),
        stats.bytes_sent.load(Ordering::Relaxed),
        stats.queue_drops.load(Ordering::Relaxed) + stats.send_errors.load(Ordering::Relaxed)
    );
    if a.remote_sink.is_some() {
        outln!("{}", receiver.summary());
        receiver.stop();
    }
    code
}

/// --sessions: the pause between Disconnect and the next Connect
const SESSION_PAUSE: Duration = Duration::from_secs(3);

/// One stream until --duration, Ctrl+C, an error or the end of the
/// session; true when --duration ended it
fn stream_session(
    a: &Args,
    service: &Service,
    rx: &mpsc::Receiver<ServiceEvent>,
    running: &AtomicBool,
    receiver: &bluecaster_core::receiver::ReceiverClient,
    code: &mut i32,
) -> bool {
    let mut streaming_since: Option<Instant> = None;
    let mut last_summary = Instant::now();
    // --headset-mic-toggle: the steps still to take
    let mut toggles: Vec<(u64, HeadsetMic)> =
        a.headset_mic_toggle.map_or(Vec::new(), |(on, off)| vec![(on, HeadsetMic::On), (off, HeadsetMic::Off)]);
    loop {
        if let Some(since) = streaming_since
            && let Some(&(at, mode)) = toggles.first()
            && since.elapsed() >= Duration::from_secs(at)
        {
            toggles.remove(0);
            let taken = service.set_headset_mic(mode, a.mic_output.clone(), a.voice_codec);
            outln!("Headset mode {} at {at} s{}", mode.as_str(), if taken { "" } else { " (not taken)" });
        }
        if a.remote_sink.is_some() && streaming_since.is_some() && last_summary.elapsed() >= Duration::from_secs(5) {
            last_summary = Instant::now();
            outln!("{}", receiver.summary());
        }
        if !running.load(Ordering::Acquire) {
            outln!("\nStopping...");
            return false;
        }
        if let (Some(since), Some(d)) = (streaming_since, a.duration)
            && since.elapsed() >= Duration::from_secs(d)
        {
            outln!("\n--duration {d} s reached");
            return true;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServiceEvent::State(state, msg)) => {
                outln!("[{state:?}] {msg}");
                if let Some(c) = msg.codec_stop {
                    outln!("Codec stop: {} {:?}", c.codec.key(), c.kind);
                }
                match state {
                    StreamState::Streaming if streaming_since.is_none() => {
                        streaming_since = Some(Instant::now());
                        if a.services {
                            print_services(service, a.device.as_deref());
                        }
                    }
                    StreamState::Error => {
                        *code = 1;
                        return false;
                    }
                    StreamState::Idle if a.wait_off && service.is_running() => {}
                    StreamState::Idle if streaming_since.is_some() || !service.is_running() => return false,
                    _ => {}
                }
            }
            Ok(ServiceEvent::StreamInfo(i)) => outln!(
                "Codec: {} | Bitrate: {} kbps | Sample rate: {} Hz | Channels: {}",
                i.codec,
                i.bitrate_kbps,
                i.sample_rate,
                i.channels
            ),
            Ok(ServiceEvent::Volume(v)) => outln!("Headphone volume: {v} ({}%)", v as u32 * 100 / 127),
            Ok(ServiceEvent::Battery(v)) => outln!("Headphone battery: {v}%"),
            Ok(ServiceEvent::BatteryRefused) => outln!("Headphone battery: unknown (HFP refused)"),
            Ok(ServiceEvent::VoiceCodecs(c)) => outln!("Headphone voice codecs: {}", c.join(", ")),
            // LE Audio runs in le_run, not here
            Ok(ServiceEvent::MicRates(_)) => {}
            Ok(ServiceEvent::OtherDeviceBack(a)) => outln!("Another device connected: {}", bt_host::addr_str(&a)),
            // The loader's own log lines say it
            Ok(ServiceEvent::Loader(_)) => {}
            // The set is kept with the saved device; merging is the UI core's
            Ok(ServiceEvent::LeSetLearned) => {}
            // Over TCP there is no USB adapter to tell
            Ok(ServiceEvent::Adapter(_)) => {}
            // Keyboards and mice have their own mode (--hid-test)
            Ok(
                ServiceEvent::HidDevices | ServiceEvent::HidPasskey | ServiceEvent::HidCheck | ServiceEvent::HidHealth,
            ) => {}
            // --ipc: the clients get the button
            Ok(ServiceEvent::Button(b)) => {
                outln!("Headphone button: {:?} {}", b.button, if b.pressed { "pressed" } else { "released" });
                if let Some(state) = IPC_STATE.get() {
                    state.on_service_event(ServiceEvent::Button(b));
                }
            }
            Ok(ServiceEvent::Voice(Some(v))) => outln!(
                "Voice: {} at {} Hz, microphone to {}",
                v.codec,
                v.rate,
                if v.mic_device_id.is_empty() { a.mic_output.as_deref().unwrap_or("-") } else { &v.mic_device_id }
            ),
            Ok(ServiceEvent::Voice(None)) => outln!("Voice: off"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// The state --ipc serves ([`serve_ipc`])
static IPC_STATE: std::sync::OnceLock<Arc<crate::state::CoreState>> = std::sync::OnceLock::new();

/// --ipc: the IPC pipe of this test instance, for the run (the emulator's
/// RFCOMM passthrough and button cases). The state the clients see is not
/// kept up to date: the service's events stay with the command line, which
/// passes on only the headphone buttons.
fn serve_ipc(service: Arc<Service>) {
    let spawned = std::thread::Builder::new().name("ipc".into()).spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
            Ok(rt) => rt,
            Err(e) => return eoutln!("IPC runtime: {e}"),
        };
        let sid = match win_sys::pipe::current_user_sid() {
            Ok(sid) => sid,
            Err(e) => return eoutln!("cannot read the user SID: {e}"),
        };
        let state = crate::state::CoreState::new(service, Box::new(|_| {}), Box::new(|| {}), Box::new(|_| {}));
        let _ = IPC_STATE.set(state.clone());
        let (name, old_name) = (ipc::pipe_name(&sid), ipc::old_pipe_name(&sid));
        outln!("IPC: serving the pipe of instance {}", ipc::instance_suffix().trim_start_matches('-'));
        if let Err(e) = rt.block_on(crate::server::serve(state, name, old_name)) {
            eoutln!("IPC server: {e}");
        }
    });
    if let Err(e) = spawned {
        eoutln!("IPC thread: {e}");
    }
}

/// --services: asked on a thread of its own, the stream goes on meanwhile
/// (and after the query each connection makes, Service::learn_services)
fn print_services(service: &Service, device: Option<&str>) {
    let (Some(host), Some(addr)) = (service.host(), device.and_then(bt_host::parse_addr)) else { return };
    let ask = move || {
        for _ in 0..40 {
            match host.device_services(addr) {
                Err(bt_host::ServicesError::Busy) => std::thread::sleep(Duration::from_millis(500)),
                r => return r,
            }
        }
        Err(bt_host::ServicesError::Busy)
    };
    let _ = std::thread::Builder::new().name("cli-services".into()).spawn(move || match ask() {
        Ok(records) => {
            outln!("Services: {} record(s)", records.len());
            for r in records {
                outln!("  {} {}", r.classes.join(" "), r.name.as_deref().unwrap_or("-"));
            }
        }
        Err(e) => outln!("Services: {e:?}"),
    });
}

// ---- LE Audio (experimental): v1 le_audio_cli.cpp ----

/// Waits for the LE core after BTstack started (v1: 50 x 100 ms)
const LE_READY_WAIT: Duration = Duration::from_secs(5);
/// le_find before each connection (v1 6000 ms)
const LE_FIND_TIMEOUT: Duration = Duration::from_secs(6);
/// le_connect (v1 30000 ms)
const LE_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Set-ups after a lost stream, in all (v1)
const LE_RESTORE_ATTEMPTS: u32 = 3;
const LE_STATS_EVERY: Duration = Duration::from_secs(5);
/// A member that left a stream the others play on is looked for this long
/// after it went, then every [`LE_REJOIN_EVERY`] (as the app's session)
const LE_REJOIN_FIRST: Duration = Duration::from_secs(3);
const LE_REJOIN_EVERY: Duration = Duration::from_secs(4);
const LE_REJOIN_SCAN: Duration = Duration::from_millis(1500);

/// The entries of Service::scan_devices, one line each
fn print_scanned(found: &[bluecaster_core::ScannedDevice]) {
    for d in found {
        let kind = match d.hid_le {
            Some(t) => {
                format!("  (LE keyboard or mouse, {})", if t == bt_host::LeAddr::PUBLIC { "public" } else { "random" })
            }
            None if d.le_audio => "  (LE Audio)".into(),
            None if d.audio => "  (audio)".into(),
            None => String::new(),
        };
        outln!("{}  {:<32} CoD 0x{:06X}  RSSI {}{kind}", d.address, d.name, d.class_of_device, d.rssi);
    }
}

/// --hid-le-device: "<address>" (public) or "<address>/random"
fn parse_hid_le_address(s: &str) -> Option<bt_host::LeAddr> {
    let (address, addr_type) = match s.trim().rsplit_once('/') {
        Some((a, t)) if t.eq_ignore_ascii_case("random") => (a, bt_host::LeAddr::RANDOM),
        Some((a, t)) if t.eq_ignore_ascii_case("public") => (a, bt_host::LeAddr::PUBLIC),
        Some(_) => return None,
        None => (s, bt_host::LeAddr::PUBLIC),
    };
    Some(bt_host::LeAddr { addr: parse_device_address(address)?, addr_type })
}

/// --hid-test: Bluetooth with keyboards and mice forced on and nothing
/// streamed; a count line a second, then a final one. Counts only: nothing
/// of what was typed or moved, and no address.
fn hid_test(a: &Args, service: &Service, secs: u64, running: &AtomicBool) -> i32 {
    let devices: Vec<[u8; 6]> = a.hid.devices.iter().filter_map(|d| parse_device_address(d)).collect();
    let le_devices: Vec<bt_host::LeAddr> = a.hid.le_devices.iter().filter_map(|d| parse_hid_le_address(d)).collect();
    // The emulator's input must never reach the machine that runs the tests
    let dry_run = !a.hid.inject || a.hci_tcp.is_some();
    if a.hid.inject && dry_run {
        outln!("--hid-inject is not used with --hci-tcp: the input is only counted");
    }
    outln!(
        "Keyboards and mice: {} allowed ({} LE), their input {}",
        devices.len() + le_devices.len(),
        le_devices.len(),
        if dry_run { "only counted" } else { "goes to Windows" }
    );
    service.set_hid_cli(devices, le_devices, dry_run);
    // The input through a real input helper that only records it (debug
    // builds): what it would inject is read back here and counted
    let recorded = if a.hid.helper_record {
        if !crate::agent::TEST_SWITCHES {
            eoutln!("--hid-helper-record is a test switch of debug builds");
            return 2;
        }
        match helper_recorder(service) {
            Ok(r) => Some(r),
            Err(e) => {
                eoutln!("the input helper's record pipe failed: {e}");
                return 1;
            }
        }
    } else {
        None
    };
    if dry_run {
        // The lights come from the command line only: the real lock keys are
        // never read
        service.set_hid_cli_leds(a.hid.leds);
    } else {
        if a.hid.leds.is_some() {
            outln!("--hid-leds is not used with --hid-inject: the lights follow Windows");
        }
        // This executable releases held keys if the test is killed
        if let Ok(exe) = std::env::current_exe() {
            service.enable_hid_guard(exe);
        }
    }
    let host = match service.ensure_host() {
        Ok(h) => h,
        Err(m) => {
            eoutln!("{m}");
            return 1;
        }
    };
    if !host.hid_enabled() {
        eoutln!("Bluetooth started without keyboards and mice");
        return 1;
    }
    if let Some(secs) = a.hid.confirm {
        service.set_hid_confirm_window(Duration::from_secs(secs));
    }
    if let Some(address) = a.hid.connect.as_deref().and_then(parse_device_address) {
        // As the app adds one: the list (here the command line's) decides
        // classic or LE, and its pairing may show a number
        if let Err(m) = service.hid_pair(&bt_host::addr_str(&address)) {
            eoutln!("hid: connecting failed ({})", m.key);
            return 1;
        }
        outln!("hid: connecting");
    }
    let counts = || match service.hid_counts() {
        Some(c) => format!(
            "connected={} reports={} events={} injected={} blocked={} undecoded={} refused={} leds={} dropped={}",
            host.hid_connected().len(),
            c.reports,
            c.events,
            c.injected,
            c.blocked,
            c.undecoded,
            c.refused,
            host.hid_counters().leds.load(Ordering::Relaxed),
            host.hid_counters().dropped.load(Ordering::Relaxed)
        ),
        None => "stopped".to_owned(),
    };
    // The number to type: printed for the emulator test only (over
    // --hci-tcp), which types it on the emulated keyboard. Never in a log.
    let mut shown: Option<String> = None;
    let mut show_passkey = || {
        let now = service.hid_passkey().map(|(_, digits)| digits);
        if now != shown {
            match (&now, a.hci_tcp.is_some()) {
                (Some(digits), true) => outln!("hid passkey shown {digits}"),
                (Some(_), false) => outln!("hid: the device asks for a number to be typed; add it in the app instead"),
                (None, _) => outln!("hid passkey cleared"),
            }
            shown = now;
        }
    };
    // --hid-confirm: the device's check, as it changes (states only)
    let mut check_state = if a.hid.confirm.is_some() { "connecting" } else { "" };
    let mut show_check = || {
        if check_state.is_empty() || check_state == "done" {
            return;
        }
        let now = match service.hid_checks().first() {
            Some(c) if c.waiting => "waiting",
            Some(_) => "connecting",
            None => "done",
        };
        if now == check_state {
            return;
        }
        match now {
            "waiting" => outln!("hid check waiting"),
            "done" => match service.hid_rolled_back() {
                Some(r) => outln!("hid check removed {}", r.reason.as_str()),
                None => outln!("hid check confirmed"),
            },
            _ => {}
        }
        check_state = now;
    };
    let start = Instant::now();
    for second in 1..=secs {
        let at = start + Duration::from_secs(second);
        while Instant::now() < at && running.load(Ordering::Acquire) {
            show_passkey();
            show_check();
            std::thread::sleep(Duration::from_millis(20));
        }
        if !running.load(Ordering::Acquire) {
            break;
        }
        outln!("hid: t={second} {}", counts());
    }
    outln!("hid final: {}", counts());
    if let Some(r) = recorded {
        let (started, connected) = service.input_helper_state().unwrap_or_default();
        let lines = r.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let inputs = lines.iter().filter(|l| !l.starts_with("exit")).count();
        let held = record_held(&lines);
        outln!("hid helper: started={started} connected={} recorded={inputs} held={held}", u8::from(connected));
    }
    0
}

/// A pipe the input helper records to (its write end inherited), read on
/// a thread into lines; the helper is enabled with it
fn helper_recorder(service: &Service) -> std::io::Result<Arc<std::sync::Mutex<Vec<String>>>> {
    use std::io::BufRead;
    let (read, write) = win_sys::input_desktop::inheritable_pipe()?;
    let number = win_sys::input_desktop::handle_number(std::os::windows::io::AsHandle::as_handle(&write));
    // Kept open for the life of this process: each helper inherits it
    std::mem::forget(write);
    let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = lines.clone();
    std::thread::Builder::new().name("helper-record".into()).spawn(move || {
        for line in std::io::BufReader::new(read).lines().map_while(Result::ok) {
            sink.lock().unwrap_or_else(|e| e.into_inner()).push(line);
        }
    })?;
    let exe = std::env::current_exe()?;
    service.enable_input_helper(exe, Some(number));
    Ok(lines)
}

/// The keys and buttons a record leaves held (downs without their up)
fn record_held(lines: &[String]) -> usize {
    let mut held = std::collections::BTreeSet::new();
    for l in lines {
        if let Some(k) = l.strip_suffix(" down") {
            held.insert(k.to_string());
        } else if let Some(k) = l.strip_suffix(" up") {
            held.remove(k);
        }
    }
    held.len()
}

fn le_print_stats(host: &bt_host::BtHost) {
    outln!("{}", bluecaster_core::le_stats_line(&host.le_stream_stats()));
}

/// What the CLI needs of the LE core (v1 le_audio::Core): [`bt_host::BtHost`],
/// or a fake in the tests
trait LeHost {
    fn le_find(&self, addr: [u8; 6], timeout: Duration) -> Option<bt_host::LeScanResult>;
    fn le_connect(&self, addr: bt_host::LeAddr, name: &str, timeout: Duration) -> Result<bt_host::LeDeviceId, String>;
}

impl LeHost for bt_host::BtHost {
    fn le_find(&self, addr: [u8; 6], timeout: Duration) -> Option<bt_host::LeScanResult> {
        bt_host::BtHost::le_find(self, addr, timeout)
    }

    fn le_connect(&self, addr: bt_host::LeAddr, name: &str, timeout: Duration) -> Result<bt_host::LeDeviceId, String> {
        bt_host::BtHost::le_connect(self, addr, name, timeout)
    }
}

/// Connects every address of the group (v1 connect_group): the address
/// type and name come from the advertisement (an RPA of a bonded device
/// changes: le_find resolves it); the ids in order, None if one fails
fn le_connect_group(
    host: &dyn LeHost,
    addrs: &[String],
    out: &mut dyn std::io::Write,
) -> Option<Vec<bt_host::LeDeviceId>> {
    let mut ids = Vec::new();
    for a in addrs {
        let Some(parsed) = parse_device_address(a) else {
            eoutln!("Invalid LE address: {a}");
            return None;
        };
        let mut addr = bt_host::LeAddr { addr: parsed, addr_type: bt_host::LeAddr::PUBLIC };
        let mut name = String::new();
        match host.le_find(parsed, LE_FIND_TIMEOUT) {
            Some(found) => {
                addr = found.addr;
                name = found.name.clone();
                let _ = writeln!(
                    out,
                    "Found {a} \"{name}\" (address type {}){}",
                    if addr.addr_type != bt_host::LeAddr::PUBLIC { "random" } else { "public" },
                    if found.le_audio { ", LE Audio" } else { "" }
                );
                if let Some(identity) = found.identity
                    && found.addr.addr != parsed
                {
                    let _ = writeln!(out, "  advertising as {} now (identity {identity})", found.addr);
                }
            }
            None => {
                let _ = writeln!(out, "{a} not seen advertising, connecting with a public address");
            }
        }
        match host.le_connect(addr, &name, LE_CONNECT_TIMEOUT) {
            Ok(id) => ids.push(id),
            Err(e) => {
                eoutln!("LE Audio: cannot connect {a}: {e}");
                return None;
            }
        }
    }
    Some(ids)
}

/// --script: checks the script as the window does (writes need
/// --allow-writes, denied opcodes never run), runs it, prints each line and
/// saves the report
fn run_script(service: &Service, file: &std::path::Path, allow_writes: bool, adapter: Option<&str>) -> i32 {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eoutln!("Cannot read {}: {e}", file.display());
            return 1;
        }
    };
    let script = match bt_host::script::parse(&text) {
        Ok(s) => s,
        Err(errors) => {
            for e in errors {
                eoutln!("{e}");
            }
            return 1;
        }
    };
    let line = |l: &bt_host::script::ScriptLine| format!("line {}: {}", l.line, l.text);
    // The vendor from -u VID:PID; a selector by instance ID or none gets no
    // deny list (the window knows the adapter)
    let vendor = adapter
        .and_then(|u| UsbSelector::from_arg(u).ok())
        .and_then(|s| s.vid_pid)
        .map_or(bt_host::chips::ChipVendor::Unknown, |(vid, pid)| bt_host::chips::vendor_for_usb(vid, pid, None));
    let denied = script.denied(bt_host::chips::denied_opcodes(vendor));
    if !denied.is_empty() {
        eoutln!("These lines could permanently change the chip and will not run on a {} adapter:", vendor.name());
        for l in denied {
            eoutln!("  {}", line(l));
        }
        return 1;
    }
    let writes = script.writes();
    if !writes.is_empty() && !allow_writes {
        eoutln!("These lines may change the chip; run with --allow-writes to send them:");
        for l in writes {
            eoutln!("  {}", line(l));
        }
        return 1;
    }
    let total = script.lines.len();
    match service.run_script(None, script) {
        Ok(report) => {
            for l in bt_host::script::report_text(&report) {
                outln!("{l}");
            }
            match &report.error {
                Some(e) => outln!("\nStopped at {e}"),
                None => outln!("\n{} of {total} lines done", report.steps.len()),
            }
            match bluecaster_core::probe_report::save_script(
                &bluecaster_core::paths::config_dir(),
                &report,
                &text,
                adapter,
                env!("CARGO_PKG_VERSION"),
            ) {
                Ok(path) => outln!("Saved to {}", path.display()),
                Err(e) => eoutln!("The report could not be saved: {e}"),
            }
            i32::from(report.error.is_some())
        }
        Err(m) => {
            eoutln!("{m}");
            1
        }
    }
}

/// v1's --le-scan listing line
fn le_scan_line(r: &bt_host::LeScanResult) -> String {
    format!(
        "  {}  {:<24}  RSSI {:4}  {}{}{}",
        r.addr,
        if r.name.is_empty() { "(no name)" } else { r.name.as_str() },
        r.rssi,
        if r.addr.addr_type != bt_host::LeAddr::PUBLIC { "random" } else { "public" },
        if r.rsi.is_some() { ", set member" } else { "" },
        if r.note.is_empty() { String::new() } else { format!(", {}", r.note) }
    )
}

/// Connects the group and starts the stream; Err: the error to print (empty
/// when it was printed already)
fn le_setup(
    host: &bt_host::BtHost,
    devices: &[String],
    request: &bt_host::LeStreamRequest,
) -> Result<bt_host::LeStreamStarted, String> {
    // (a failed connection printed why)
    let ids = le_connect_group(host, devices, &mut std::io::stdout()).ok_or_else(String::new)?;
    host.le_start_stream(ids, request.clone(), &bluecaster_core::le_silence_set)
}

/// The LE Audio source of the command line: the test tone, a virtual
/// device, or system audio
fn le_source(a: &Args) -> SourceRequest {
    if a.test_tone {
        SourceRequest::TestTone
    } else if a.mode == "virtual" && !a.audio_device.is_empty() {
        SourceRequest::VirtualDevice { id: a.audio_device.clone(), name: a.audio_device.clone(), auto_switch: false }
    } else {
        SourceRequest::SystemLoopback
    }
}

/// --auracast: broadcast until --duration or Ctrl+C, through the service as
/// the app's stream.start_broadcast does; the exit code
fn le_broadcast(a: &Args, service: &Service, rx: &mpsc::Receiver<ServiceEvent>, running: &AtomicBool) -> i32 {
    let le = &a.le;
    let b = crate::state::BroadcastStream {
        name: le.broadcast.clone().unwrap_or_default(),
        code: le.broadcast_code.clone(),
        quality: "hq".into(),
        mono: Some(le.broadcast_mono),
        layout: le.broadcast_layout.clone(),
        source: "tone".into(),
        ..Default::default()
    };
    let mut request = match crate::state::broadcast_request(&b, bt_host::media::MEDIA_PAYLOAD_LIMIT_DEFAULT) {
        Ok(r) => r,
        Err(e) => {
            eoutln!("Auracast: {} {:?}", e.code, e.message.args);
            return 2;
        }
    };
    request.source = le_source(a);
    if let (Some(p), Some(l)) = (&le.preset, request.le.as_mut()) {
        l.preset = Some(p.clone());
    }
    if let (Some(p), Some(b)) = (&le.broadcast_rear_preset, request.le.as_mut().and_then(|l| l.broadcast.as_mut())) {
        b.rear_preset = Some(p.clone());
    }
    outln!("Starting the broadcast \"{}\"...", b.name.trim());
    service.start_streaming(request);
    let mut code = 0;
    let receiver = bluecaster_core::receiver::ReceiverClient::new();
    stream_session(a, service, rx, running, &receiver, &mut code);
    if let Some(host) = service.host() {
        le_print_stats(&host);
    }
    code
}

/// The LE Audio flow (v1 le_audio_cli_run); the process exit code
fn le_run(a: &Args, service: &Service, running: &Arc<AtomicBool>) -> i32 {
    let le = &a.le;
    service.set_le_audio_cli(bluecaster_core::LeCliOptions {
        mcp_dry_run: le.mcp_dry_run,
        ccp_sim: le.ccp_sim.clone(),
        ccp_console: le.ccp_console,
    });
    let is_running = || running.load(Ordering::Acquire);

    outln!(
        "\n[LE Audio, experimental] Initializing BTstack ({} transport)...",
        if a.hci_tcp.is_none() { "WinUSB" } else { "H4 over TCP" }
    );
    let host = match service.ensure_host() {
        Ok(h) => h,
        Err(m) => {
            tracing::error!("{m}");
            eoutln!("Failed to initialize BTstack.");
            return 1;
        }
    };
    // Ctrl+C ends the blocking LE calls (find, connect, stream set-up)
    {
        let canceller = host.canceller();
        let running = running.clone();
        std::thread::Builder::new()
            .name("le-cancel".into())
            .spawn(move || {
                while running.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                canceller.cancel();
            })
            .ok();
    }
    let until = Instant::now() + LE_READY_WAIT;
    while !host.le_ready() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
    if !host.le_ready() {
        eoutln!("LE Audio did not start (does the adapter support LE?)");
        return 1;
    }

    if let Some(secs) = le.scan {
        outln!("Scanning for LE Audio devices ({secs} s)...");
        let results = host.le_scan(Duration::from_secs(secs.into()));
        let mut n = 0;
        for r in results.iter().filter(|r| r.le_audio) {
            n += 1;
            outln!("{}", le_scan_line(r));
        }
        outln!("{n} LE Audio device(s) found");
        return 0;
    }

    if let Some(secs) = le.qcy_probe {
        let address = match a.device.as_deref().map(|d| (d, parse_device_address(d))) {
            Some((d, None)) => {
                eprintln!("Invalid address: {d}");
                return 2;
            }
            Some((_, parsed)) => parsed,
            None => None,
        };
        return crate::qcy_probe::run(&host, secs, address);
    }

    let devices: Vec<String> = a.device.iter().chain(le.devices.iter()).cloned().collect();
    if devices.is_empty() {
        eoutln!("No LE Audio device given (-d <address>; --le-scan lists them)");
        return 1;
    }

    // The headset microphone (--headset-mic): its output resolved and
    // checked as in the app, before the stream asks for it
    let source = le_source(a);
    let mic_target = match bluecaster_core::le_mic_target(a.headset_mic.is_on(), a.mic_output.as_deref(), &source) {
        Ok(t) => t,
        Err(m) => {
            outln!("Microphone: not used ({m})");
            None
        }
    };
    let (mut request, low_latency, note) = le.request();
    request.conversational = mic_target.is_some();
    request.source_rate = a.mic_rate;
    host.le_set_low_latency(low_latency);
    if let Some(note) = note {
        outln!("{note}");
    }

    outln!("\nConnecting {} LE Audio device(s)...", devices.len());
    let started = match le_setup(&host, &devices, &request) {
        Ok(s) => s,
        Err(e) => {
            if !e.is_empty() {
                eoutln!("LE Audio: cannot stream: {e}");
            }
            host.le_stop_stream();
            host.le_disconnect_all();
            return 1;
        }
    };
    outln!("\nStreaming LE Audio: {}", started.status.summary);

    // Audio at the LC3 rate (WASAPI converts to it)
    let mut rc = 0;
    let start_audio = |started: &bt_host::LeStreamStarted| match bluecaster_core::LeAudioPath::start(
        &source,
        started,
        host.stats().clone(),
        None,
    ) {
        Ok(path) => {
            let rate = path.rate();
            if a.test_tone {
                outln!("Audio: test tone (1 kHz left / 1.5 kHz right) at {rate} Hz");
            } else {
                let f = path.format();
                outln!("Audio: WASAPI capture {} Hz {} ch (LC3 at {rate} Hz)", f.sample_rate, f.channels);
            }
            Some(path)
        }
        Err(e) => {
            tracing::error!("LE Audio: audio: {e}");
            eoutln!("Failed to start audio capture");
            None
        }
    };
    let mut audio = start_audio(&started);
    if audio.is_none() {
        rc = 1;
    }
    // The headset microphone (--headset-mic)
    let mut started = started;
    let mut mic_streams = 1;
    let start_mic = |started: &mut bt_host::LeStreamStarted, n: u32| {
        let target = mic_target.as_ref()?;
        let dsp = bluecaster_core::MicDsp::new();
        bluecaster_core::apply_mic_processing(&dsp, &a.mic_processing);
        match bluecaster_core::start_mic(started, target, n, dsp) {
            Ok(Some(m)) => {
                outln!("Microphone: LC3 at {} Hz -> {}", m.rate(), m.target());
                Some(m)
            }
            Ok(None) => {
                let note = &started.status.plan.source_note;
                outln!("Microphone: none in the stream ({})", if note.is_empty() { "not set up" } else { note });
                None
            }
            Err(e) => {
                outln!("Microphone: the output did not open ({e})");
                None
            }
        }
    };
    let mut mic = start_mic(&mut started, mic_streams);

    // Test only: the headset volume synced with a simulated Windows volume
    let mut sim_volume = None;
    if let Some(spec) = &le.sim_volume {
        match bluecaster_core::volume_sync::SimulatedWindowsVolume::parse(spec) {
            Some(v) => sim_volume = Some(v),
            None => {
                eoutln!("Invalid --le-sim-volume {spec}");
                rc = 1;
            }
        }
    }

    let start = Instant::now();
    let mut last_print = start;
    let mut restarts = 0;
    let (mut shown_dropped, mut rejoin_at) = (0, None);
    while rc == 0 && is_running() {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(d) = a.duration
            && start.elapsed() >= Duration::from_secs(d)
        {
            outln!("\n--duration {d} s reached");
            break;
        }
        if last_print.elapsed() >= LE_STATS_EVERY {
            last_print = Instant::now();
            le_print_stats(&host);
        }
        if let Some(v) = sim_volume.as_mut() {
            v.tick(&bluecaster_core::volume_sync::LeHeadphones(&host));
        }
        // A member left while the others play on: looked for, then the
        // stream is set up again with it
        let dropped = host.le_stream_dropped();
        if dropped.len() != shown_dropped {
            shown_dropped = dropped.len();
            let playing = started.status.devices.iter().filter(|d| !dropped.contains(d)).count();
            if shown_dropped > 0 && playing > 0 {
                outln!("\n*** LE Audio: {shown_dropped} device(s) left the stream, {playing} play on ***");
                if let Some(p) = audio.as_ref() {
                    p.set_mono(playing <= 1);
                }
                rejoin_at = Some(Instant::now() + LE_REJOIN_FIRST);
            }
        }
        let lost = host.le_stream_lost();
        let mut member_back = false;
        if !lost && rejoin_at.is_some_and(|t| Instant::now() >= t) {
            let known = host.le_devices();
            let gone: Vec<_> = known.iter().filter(|d| dropped.contains(&d.id)).collect();
            let addrs: Vec<[u8; 6]> = gone.iter().map(|d| d.addr.addr).collect();
            // Still connected (only its CIS or ASE ended), or advertising again
            member_back = gone.iter().any(|d| d.connected)
                || host.le_find_any_quietly(&addrs, LE_REJOIN_SCAN).is_some_and(|(_, seen)| {
                    outln!("*** LE Audio: {} is back - setting the stream up with it ***", seen.addr);
                    true
                });
            rejoin_at = (!member_back).then(|| Instant::now() + LE_REJOIN_EVERY);
        }
        if lost || member_back {
            // A device or CIS went away: tear down and set it up again
            if lost {
                outln!("\n*** LE Audio stream lost - setting it up again ***");
            }
            (shown_dropped, rejoin_at) = (0, None);
            if let Some(p) = audio.take() {
                p.stop();
            }
            host.le_stop_stream();
            // After the stream stopped: what arrived until then is decoded
            if let Some(m) = mic.take() {
                m.stop();
            }
            let mut again = None;
            while again.is_none() && restarts < LE_RESTORE_ATTEMPTS && is_running() {
                restarts += 1;
                std::thread::sleep(Duration::from_secs(2));
                match le_setup(&host, &devices, &request) {
                    Ok(s) => again = Some(s),
                    Err(e) => {
                        if !e.is_empty() {
                            tracing::warn!("LE Audio: set-up failed: {e}");
                        }
                        host.le_stop_stream();
                    }
                }
            }
            let Some(s) = again else {
                eoutln!("LE Audio: could not restore the stream");
                rc = 1;
                break;
            };
            started = s;
            outln!("*** LE Audio stream restored: {} ***", started.status.summary);
            audio = start_audio(&started);
            mic_streams += 1;
            mic = start_mic(&mut started, mic_streams);
            if audio.is_none() {
                rc = 1;
            }
        }
    }

    outln!("\nShutting down...");
    if let Some(p) = audio.take() {
        p.stop();
    }
    std::thread::sleep(Duration::from_millis(100));
    le_print_stats(&host);
    host.le_stop_stream();
    // After the stream stopped (Disable ends the receiving): every SDU that
    // arrived is decoded
    if let Some(m) = mic.take() {
        let c = m.stop();
        outln!("{}", c.message());
    }
    host.le_disconnect_all();
    service.shutdown();
    outln!("Done.");
    rc
}

/// v1's listing of the Windows-paired devices (BtDeviceDiscovery::
/// scan_paired_devices)
fn print_paired(devices: &[PairedDevice]) {
    if devices.is_empty() {
        outln!("No paired Bluetooth devices found");
        return;
    }
    for d in devices {
        outln!("  [{}] {} ({})", if d.is_a2dp_sink() { "A2DP" } else { "----" }, d.name, d.address_string());
    }
    outln!("Found {} devices ({} with A2DP Sink)", devices.len(), devices.iter().filter(|d| d.is_a2dp_sink()).count());
}

/// A Bluetooth address as v1 took it (BtDeviceDiscovery::normalize_address):
/// 12 hex digits, optionally with ':', '-', '.' or ' ' between the pairs
fn parse_device_address(s: &str) -> Option<[u8; 6]> {
    let mut digits = Vec::with_capacity(12);
    let mut after_sep = false;
    for c in s.trim().chars() {
        if let Some(v) = c.to_digit(16) {
            if digits.len() == 12 {
                return None;
            }
            digits.push(v as u8);
            after_sep = false;
        } else if matches!(c, ':' | '-' | '.' | ' ') {
            // Only between pairs, and not two in a row
            if digits.is_empty() || digits.len() % 2 != 0 || after_sep {
                return None;
            }
            after_sep = true;
        } else {
            return None;
        }
    }
    if digits.len() != 12 || after_sep {
        return None;
    }
    let mut a = [0u8; 6];
    for (i, b) in a.iter_mut().enumerate() {
        *b = digits[2 * i] << 4 | digits[2 * i + 1];
    }
    Some(a)
}

/// --eq's points: `<Hz>:<dB>[:<Q>[:<type>]]`, comma separated
fn parse_eq(v: &str) -> Result<Vec<dsp::EqBand>, String> {
    let bad = || format!("bad --eq point in \"{v}\" (<Hz>:<dB>[:<Q>[:<type>]])");
    v.split(',')
        .map(|point| {
            let mut parts = point.trim().split(':');
            let mut num = |default: Option<f32>| match parts.next() {
                Some(x) => x.trim().parse::<f32>().ok().filter(|n| n.is_finite()),
                None => default,
            };
            let (frequency, gain, q) =
                (num(None).ok_or_else(bad)?, num(None).ok_or_else(bad)?, num(Some(1.0)).ok_or_else(bad)?);
            let band_type = match parts.next().map(|t| t.trim().to_ascii_lowercase()).as_deref() {
                None | Some("peak") => dsp::BandType::Peaking,
                Some("lowshelf") => dsp::BandType::LowShelf,
                Some("highshelf") => dsp::BandType::HighShelf,
                Some("highpass") => dsp::BandType::HighPass,
                Some("lowpass") => dsp::BandType::LowPass,
                Some(_) => return Err(bad()),
            };
            Ok(dsp::EqBand { frequency, gain, q, band_type })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn list_and_adapter_options() {
        let a = parse(&args("--cli -l")).unwrap();
        assert!(a.list && a.usb.is_none() && a.device.is_none());

        let a = parse(&args(r"--cli -u USB\VID_0BDA&PID_8771\00E04C000001 -d 00:11:22:33:44:55")).unwrap();
        let (given, sel) = a.usb.unwrap();
        assert_eq!(given, r"USB\VID_0BDA&PID_8771\00E04C000001");
        assert_eq!(sel.instance_id.as_deref(), Some(r"USB\VID_0BDA&PID_8771\00E04C000001"));

        let a = parse(&args(r"--cli -u \\?\usb#vid_0bda&pid_8771#00e04c000001#{a5dcbf10-6530-11d2-901f-00c04fb951ed}"))
            .unwrap();
        assert_eq!(a.usb.unwrap().1.instance_id.as_deref(), Some(r"usb\vid_0bda&pid_8771\00e04c000001"));

        let a = parse(&args("--cli -u 0bda:8771")).unwrap();
        assert_eq!(a.usb.unwrap().1.vid_pid, Some((0x0BDA, 0x8771)));

        assert!(parse(&args("--cli -u nonsense")).err().unwrap_or_default().contains("not a USB device"));
        assert_eq!(parse(&args("--cli -u")).err().as_deref(), Some("-u needs a value"));
    }

    #[test]
    fn session_agent_option() {
        assert!(!parse(&args("--cli --test-tone")).unwrap().session_agent);
        assert!(parse(&args("--cli --test-tone --session-agent")).unwrap().session_agent);
        // The stage 1 name still works
        assert!(parse(&args("--cli --test-tone --capture-agent")).unwrap().session_agent);
    }

    #[test]
    fn sbc_options() {
        use codec::sbc::{Allocation, ChannelMode, SbcParams};
        let a = parse(&args("--cli -c sbc")).unwrap();
        assert_eq!((a.sbc, a.sbc_bitpool), (SbcParams::default(), 0));
        let a = parse(&args(
            "--cli -c sbc --sbc-blocks 8 --sbc-subbands 4 --sbc-alloc SNR --sbc-mode stereo --sbc-bitpool 40",
        ))
        .unwrap();
        assert_eq!(
            a.sbc,
            SbcParams {
                blocks: 8,
                subbands: 4,
                allocation: Some(Allocation::Snr),
                channel_mode: Some(ChannelMode::Stereo),
                min_bitpool: 0,
            }
        );
        assert_eq!(a.sbc_bitpool, 40);
        for bad in [
            "--sbc-blocks 6",
            "--sbc-subbands 2",
            "--sbc-alloc max",
            "--sbc-mode auto",
            "--sbc-mode dual_channel",
            "--sbc-bitpool 60",
        ] {
            assert!(parse(&args(&format!("--cli {bad}"))).is_err(), "{bad}");
        }
    }

    #[test]
    fn le_audio_options() {
        // As run_test.py --le-audio calls it
        let a = parse(&args(
            "--cli --hci-tcp 127.0.0.1:9610 --le-audio -d 00:A2:D0:00:00:12 --le-device 00:A2:D0:00:00:13 \
             --test-tone --duration 5 --hci-capture out\\bluecaster.pklg --le-sim-volume 64,3:90",
        ))
        .unwrap();
        assert!(a.le.on && a.test_tone);
        assert_eq!(a.device.as_deref(), Some("00:A2:D0:00:00:12"));
        assert_eq!(a.le.devices, ["00:A2:D0:00:00:13"]);
        assert_eq!(a.le.sim_volume.as_deref(), Some("64,3:90"));
        assert_eq!(a.duration, Some(5));
        assert_eq!(a.hci_tcp.as_deref(), Some("127.0.0.1:9610"));

        // --le-device alone implies --le-audio, and repeats
        let a = parse(&args("--cli --le-device aa:bb:cc:dd:ee:01 --le-device aa:bb:cc:dd:ee:02")).unwrap();
        assert!(a.le.on);
        assert_eq!(a.le.devices.len(), 2);
        assert!(!parse(&args("--cli -d aa:bb:cc:dd:ee:01")).unwrap().le.on);

        let a = parse(&args("--cli --le-audio --le-preset 16_2_1 --mcp-dry-run --ccp-console")).unwrap();
        assert_eq!(a.le.preset.as_deref(), Some("16_2_1"));
        assert!(a.le.mcp_dry_run && a.le.ccp_console && !a.le.low_latency);
        let a = parse(&[
            "--cli".into(),
            "--le-audio".into(),
            "--ccp-sim".into(),
            "1:incoming tel:+1555|Alice|tel:+1666;4:hangup".into(),
        ])
        .unwrap();
        assert_eq!(a.le.ccp_sim.as_deref(), Some("1:incoming tel:+1555|Alice|tel:+1666;4:hangup"));
        assert!(a.headset_mic == HeadsetMic::Off && a.mic_output.is_none());

        // The headset microphone (run_test.py's mic-* cases)
        let a = parse(&args("--cli --le-audio -d 00:A2:D0:00:00:12 --headset-mic --mic-output out\\mic.wav")).unwrap();
        assert!(a.le.on && a.headset_mic == HeadsetMic::On);
        assert_eq!(a.mic_output.as_deref(), Some("out\\mic.wav"));
        // A mode after it: on or auto; anything else is the next argument
        let a = parse(&args("--cli --headset-mic auto --mic-output out\\mic.wav")).unwrap();
        assert_eq!((a.headset_mic, a.mic_output.as_deref()), (HeadsetMic::Auto, Some("out\\mic.wav")));
        assert_eq!(parse(&args("--cli --headset-mic on")).unwrap().headset_mic, HeadsetMic::On);
        assert_eq!(parse(&args("--cli --headset-mic")).unwrap().headset_mic, HeadsetMic::On);
        assert_eq!(parse(&args("--cli --mic-output")).err().as_deref(), Some("--mic-output needs a value"));
        let a = parse(&args("--cli --headset-mic --mic-gain 6 --mic-noise-suppression")).unwrap();
        assert_eq!((a.mic_processing.enabled, a.mic_processing.gain_db, a.mic_processing.suppression), (true, 6, 100));
        assert_eq!(parse(&args("--cli --mic-gain 30")).err().as_deref(), Some("--mic-gain takes -12 to 24 (dB)"));
        // The microphone's codec (run_test.py's voice-limit cases)
        let a = parse(&args("--cli --headset-mic --voice-codec msbc --mic-rate 16000")).unwrap();
        assert_eq!((a.voice_codec, a.mic_rate), (Some(bluecaster_core::VoiceCodec::Msbc), Some(16000)));
        assert!(parse(&args("--cli --voice-codec lc3")).is_err());
        assert!(parse(&args("--cli --mic-rate x")).is_err());

        assert_eq!(parse(&args("--cli --le-device")).err().as_deref(), Some("--le-device needs a value"));
        assert_eq!(parse(&args("--cli --le-preset")).err().as_deref(), Some("--le-preset needs a value"));
        assert_eq!(parse(&args("--cli --le-sim-volume")).err().as_deref(), Some("--le-sim-volume needs a value"));
        assert_eq!(parse(&args("--cli --ccp-sim")).err().as_deref(), Some("--ccp-sim needs a value"));
    }

    #[test]
    fn qcy_probe_takes_optional_seconds_and_implies_le() {
        let a = parse(&args("--cli --qcy-probe")).unwrap();
        assert!(a.le.on);
        assert_eq!(a.le.qcy_probe, Some(QCY_PROBE_SECONDS));
        assert_eq!(parse(&args("--cli --qcy-probe 4")).unwrap().le.qcy_probe, Some(4));
        let a = parse(&args("--cli --qcy-probe -d C4:33:96:11:0C:83")).unwrap();
        assert_eq!(a.le.qcy_probe, Some(QCY_PROBE_SECONDS));
        assert_eq!(a.device.as_deref(), Some("C4:33:96:11:0C:83"));
        assert_eq!(parse(&args("--cli --le-scan")).unwrap().le.qcy_probe, None);
    }

    #[test]
    fn le_scan_takes_optional_seconds() {
        let a = parse(&args("--cli --le-scan")).unwrap();
        assert!(a.le.on);
        assert_eq!(a.le.scan, Some(LE_SCAN_SECONDS));
        assert_eq!(parse(&args("--cli --le-scan 3")).unwrap().le.scan, Some(3));
        let a = parse(&args("--cli --le-scan --hci-tcp 127.0.0.1:1")).unwrap();
        assert_eq!(a.le.scan, Some(LE_SCAN_SECONDS));
        assert_eq!(a.hci_tcp.as_deref(), Some("127.0.0.1:1"));
        // The classic --scan is another option
        let a = parse(&args("--cli --scan 4")).unwrap();
        assert_eq!((a.scan, a.le.on), (Some(4), false));
    }

    #[test]
    fn hid_test_options() {
        let a = parse(&args(
            "--cli --hci-tcp 127.0.0.1:1 --hid-test 12 --hid-device 00:A2:D0:00:00:03 \
             --hid-device 00:A2:D0:00:00:04 --hid-connect 00:a2:d0:00:00:03 --hid-inject",
        ))
        .unwrap();
        assert_eq!(a.hid.test, Some(12));
        assert_eq!(a.hid.devices.len(), 2);
        assert_eq!(a.hid.connect.as_deref(), Some("00:a2:d0:00:00:03"));
        assert!(a.hid.inject);
        assert!(parse(&args("--cli --hid-test 0")).is_err());
        assert!(parse(&args("--cli --hid-device 00:A2:D0:00:00:03")).is_err());
        assert!(parse(&args("--cli --hid-test 5 --hid-device 00:A2:D0:00:00")).is_err());
        // Connecting needs the device on the list
        assert!(parse(&args("--cli --hid-test 5 --hid-connect 00:A2:D0:00:00:03")).is_err());
        // LE devices, public or random
        let a = parse(&args(
            "--cli --hid-test 5 --hid-le-device 00:A2:D0:00:00:05 --hid-le-device C0:A2:D0:00:00:06/random \
             --hid-connect c0:a2:d0:00:00:06",
        ))
        .unwrap();
        let le: Vec<_> = a.hid.le_devices.iter().filter_map(|d| parse_hid_le_address(d)).collect();
        assert_eq!(le.iter().map(|l| l.addr_type).collect::<Vec<_>>(), [0, 1]);
        assert!(parse(&args("--cli --hid-test 5 --hid-le-device 00:A2:D0:00:00:05/static")).is_err());
        assert!(parse(&args("--cli --hid-le-device 00:A2:D0:00:00:05")).is_err());
        assert_eq!(parse(&args("--cli --hid-scan 6")).unwrap().hid.scan, Some(6));
        assert!(parse(&args("--cli --hid-scan 6 --hid-test 5")).is_err());
        let a = parse(&args(
            "--cli --hid-test 5 --hid-device 00:A2:D0:00:00:03 --hid-connect 00:A2:D0:00:00:03 --hid-confirm 4",
        ))
        .unwrap();
        assert_eq!(a.hid.confirm, Some(4));
        assert!(parse(&args("--cli --hid-test 5 --hid-device 00:A2:D0:00:00:03 --hid-confirm 4")).is_err());
        assert!(
            parse(&args(
                "--cli --hid-test 5 --hid-device 00:A2:D0:00:00:03 --hid-connect 00:A2:D0:00:00:03 --hid-confirm 0"
            ))
            .is_err()
        );
    }

    #[test]
    fn auracast_options() {
        let a =
            parse(&args("--cli --hci-tcp 127.0.0.1:9900 --auracast BlueCaster-test --test-tone --duration 5")).unwrap();
        assert!(a.le.on && a.test_tone && a.device.is_none());
        assert_eq!(a.le.broadcast.as_deref(), Some("BlueCaster-test"));
        assert!(a.le.broadcast_code.is_none() && !a.le.broadcast_mono && a.le.broadcast_layout.is_none());
        let layout = |s: &str| parse(&args(s)).map(|a| a.le.broadcast_layout);
        assert_eq!(layout("--cli --auracast Speakers --auracast-split").unwrap().as_deref(), Some("split"));
        assert_eq!(layout("--cli --auracast Speakers --auracast-layout quad").unwrap().as_deref(), Some("quad"));
        assert!(layout("--cli --auracast Speakers --auracast-layout 5.1").is_err());
        let a =
            parse(&args("--cli --auracast Speakers --auracast-layout pairs --auracast-rear-preset 24_2_2")).unwrap();
        assert_eq!(a.le.broadcast_rear_preset.as_deref(), Some("24_2_2"));
        let a = parse(&[
            "--cli".into(),
            "--auracast".into(),
            "Living room".into(),
            "--auracast-code".into(),
            "1234abcd".into(),
            "--auracast-mono".into(),
            "--le-preset".into(),
            "24_2_2".into(),
        ])
        .unwrap();
        assert_eq!(a.le.broadcast.as_deref(), Some("Living room"));
        assert_eq!(a.le.broadcast_code.as_deref(), Some("1234abcd"));
        assert!(a.le.broadcast_mono);
        assert_eq!(a.le.preset.as_deref(), Some("24_2_2"));
        assert_eq!(parse(&args("--cli --auracast")).err().as_deref(), Some("--auracast needs a value"));
        // Mono and split exclude each other, in either order
        for both in ["--auracast-mono --auracast-split", "--auracast-split --auracast-mono"] {
            assert_eq!(
                parse(&args(&format!("--cli --auracast Speakers {both}"))).err().as_deref(),
                Some("--auracast-mono and --auracast-split cannot be used together")
            );
        }
    }

    #[test]
    fn le_preset_overrides_low_latency() {
        let a = parse(&args("--cli --le-audio --le-low-latency")).unwrap();
        let (request, low_latency, note) = a.le.request();
        assert!(low_latency && note.is_none());
        assert_eq!(request, bt_host::LeStreamRequest::default());

        let a = parse(&args("--cli --le-audio --le-low-latency --le-preset 48_2_2")).unwrap();
        let (request, low_latency, note) = a.le.request();
        assert!(!low_latency);
        assert_eq!(note, Some("--le-preset given: --le-low-latency is not used"));
        assert_eq!(request.preset_order, ["48_2_2"]);

        let (_, low_latency, note) = parse(&args("--cli --le-audio")).unwrap().le.request();
        assert!(!low_latency && note.is_none());
    }

    #[test]
    fn every_le_case_of_run_test_parses() {
        // tools/emu/le_audio_tests: bluecaster_args after the common ones
        for extra in [
            &["--afh", "6,11"][..],
            &["--ccp-sim", "2:alerting;3:answered"],
            &["--le-low-latency"],
            &["--le-preset", "16_2_1"],
            &["--le-sim-volume", "64"],
            &["--mcp-dry-run"],
        ] {
            let mut v = args("--cli --hci-tcp 127.0.0.1:9600 --le-audio -d 00:A2:D0:00:00:12 --test-tone --duration 8");
            v.extend(["--hci-capture".to_string(), r"C:\out dir\bluecaster.pklg".to_string()]);
            v.extend(extra.iter().map(|s| s.to_string()));
            let a = parse(&v).unwrap_or_else(|e| panic!("{extra:?}: {e}"));
            assert!(a.le.on, "{extra:?}");
        }
    }

    /// The LE core of the tests: devices advertise as listed
    struct FakeLe {
        adverts: Vec<bt_host::LeScanResult>,
        refuse: Option<[u8; 6]>,
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl LeHost for FakeLe {
        fn le_find(&self, addr: [u8; 6], _timeout: Duration) -> Option<bt_host::LeScanResult> {
            self.calls.borrow_mut().push(format!("find {}", bt_host::addr_str(&addr)));
            self.adverts.iter().find(|r| r.addr.addr == addr || r.identity.is_some_and(|i| i.addr == addr)).cloned()
        }
        fn le_connect(
            &self,
            addr: bt_host::LeAddr,
            name: &str,
            _timeout: Duration,
        ) -> Result<bt_host::LeDeviceId, String> {
            self.calls.borrow_mut().push(format!("connect {addr} type {} \"{name}\"", addr.addr_type));
            if self.refuse == Some(addr.addr) {
                return Err("no answer".into());
            }
            Ok(bt_host::LeDeviceId(self.calls.borrow().len() as u32))
        }
    }

    #[test]
    fn le_group_is_found_then_connected_in_order() {
        let rpa = bt_host::LeAddr { addr: [0x5A, 1, 2, 3, 4, 5], addr_type: bt_host::LeAddr::RANDOM };
        let identity = bt_host::LeAddr { addr: [0x00, 0xA2, 0xD0, 0, 0, 0x12], addr_type: bt_host::LeAddr::PUBLIC };
        let le = FakeLe {
            adverts: vec![bt_host::LeScanResult {
                addr: rpa,
                identity: Some(identity),
                name: "Buds L".into(),
                le_audio: true,
                ..Default::default()
            }],
            refuse: None,
            calls: Default::default(),
        };
        let mut out = Vec::new();
        let ids = le_connect_group(&le, &["00:a2:d0:00:00:12".into(), "00:A2:D0:00:00:13".into()], &mut out).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(
            *le.calls.borrow(),
            [
                "find 00:A2:D0:00:00:12",
                "connect 5A:01:02:03:04:05 type 1 \"Buds L\"",
                "find 00:A2:D0:00:00:13",
                "connect 00:A2:D0:00:00:13 type 0 \"\""
            ]
        );
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text,
            "Found 00:a2:d0:00:00:12 \"Buds L\" (address type random), LE Audio\n  \
             advertising as 5A:01:02:03:04:05 now (identity 00:A2:D0:00:00:12)\n\
             00:A2:D0:00:00:13 not seen advertising, connecting with a public address\n"
        );

        // A failed connection ends the group; a bad address is refused first
        let le = FakeLe { adverts: Vec::new(), refuse: Some([0, 0xA2, 0xD0, 0, 0, 0x12]), calls: Default::default() };
        assert!(
            le_connect_group(&le, &["00:A2:D0:00:00:12".into(), "00:A2:D0:00:00:13".into()], &mut Vec::new()).is_none()
        );
        assert_eq!(le.calls.borrow().len(), 2);
        assert!(le_connect_group(&le, &["nonsense".into()], &mut Vec::new()).is_none());
    }

    #[test]
    fn le_scan_lines_as_v1() {
        let r = bt_host::LeScanResult {
            addr: bt_host::LeAddr { addr: [0x00, 0xA2, 0xD0, 0, 0, 0x12], addr_type: 0 },
            name: "Buds".into(),
            rssi: -40,
            rsi: Some([0; 6]),
            note: "CSIP set 0".into(),
            le_audio: true,
            ..Default::default()
        };
        assert_eq!(
            le_scan_line(&r),
            "  00:A2:D0:00:00:12  Buds                      RSSI  -40  public, set member, CSIP set 0"
        );
        let r = bt_host::LeScanResult { name: String::new(), rsi: None, note: String::new(), ..r };
        assert_eq!(le_scan_line(&r), "  00:A2:D0:00:00:12  (no name)                 RSSI  -40  public");
    }

    #[test]
    fn device_address_as_v1() {
        let want = Some([0x00, 0x1A, 0x7D, 0xDA, 0x71, 0x13]);
        for ok in ["00:1A:7D:DA:71:13", "00-1a-7d-da-71-13", "001A7DDA7113", "00.1A.7D.DA.71.13", " 00 1A 7D DA 71 13 "]
        {
            assert_eq!(parse_device_address(ok), want, "{ok}");
        }
        for bad in [
            "",
            "00:1A:7D:DA:71",
            "00:1A:7D:DA:71:13:00",
            "0:01A:7D:DA:71:13",
            "00::1A7DDA7113",
            "00:1A:7D:DA:71:13:",
            "00:1A:7D:DA:71:1G",
        ] {
            assert_eq!(parse_device_address(bad), None, "{bad}");
        }
    }

    #[test]
    fn eq_points() {
        let a = parse(&args("--cli -d 00:A2:D0:00:00:12 --eq 1000:6:4,80:-3:0.7:lowshelf,30:0")).unwrap();
        assert_eq!(
            a.eq,
            [
                dsp::EqBand::peaking(1000.0, 6.0, 4.0),
                dsp::EqBand { frequency: 80.0, gain: -3.0, q: 0.7, band_type: dsp::BandType::LowShelf },
                dsp::EqBand::peaking(30.0, 0.0, 1.0),
            ]
        );
        assert!(parse(&args("--cli --eq 1000")).is_err());
        assert!(parse(&args("--cli --eq 1000:3:1:wobble")).is_err());
    }
}
