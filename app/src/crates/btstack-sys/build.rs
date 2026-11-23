//! Builds BTstack (classic: HCI, L2CAP, SDP, AVDTP / A2DP source, AVRCP; LE:
//! SM, ATT / GATT, LE device DB in a TLV file, ISO), the Windows run loop and
//! TLV store, micro-ecc, the chipset drivers and the C glue in csrc/shim.c
//! with the MSVC compiler found by `cc`.
//!
//! The BTstack sources come from the git submodule extern/btstack of the
//! repository (override with the BTSTACK_DIR environment variable).

use std::env;
use std::path::{Path, PathBuf};

/// src/*.c not needed by BlueCaster (audio playback helpers, LC3 - LE Audio
/// encodes with crate lc3-sys -, other HCI transports, HID)
const CORE_EXCLUDE: &[&str] = &[
    "btstack_audio.c",
    "btstack_audio_generator.c",
    "btstack_audio_player.c",
    "btstack_audio_song.c",
    "btstack_lc3.c",
    "btstack_lc3_google.c",
    "btstack_lc3plus_fraunhofer.c",
    "btstack_resample.c",
    "btstack_sample_rate_compensation.c",
    "btstack_slip.c",
    "btstack_uart_slip_wrapper.c",
    "hci_transport_em9304_spi.c",
    "hci_transport_h4.c",
    "hci_transport_h5.c",
    "btstack_hid.c",
    "btstack_hid_parser.c",
    "btstack_base64_decoder.c",
    "btstack_base64_encoder.c",
];

const CLASSIC: &[&str] = &[
    "a2dp.c",
    "a2dp_source.c",
    "a2dp_sink.c",
    "avdtp.c",
    "avdtp_acceptor.c",
    "avdtp_initiator.c",
    "avdtp_sink.c",
    "avdtp_source.c",
    "avdtp_util.c",
    "avrcp.c",
    "avrcp_controller.c",
    "avrcp_target.c",
    "rfcomm.c",
    "hfp.c",
    "hfp_ag.c",
    "hfp_gsm_model.c",
    "hid_host.c",
    "sdp_client.c",
    "sdp_client_rfcomm.c",
    "sdp_server.c",
    "sdp_util.c",
    "btstack_link_key_db_memory.c",
];

/// src/ble/*.c not built: LE bonds are kept in a TLV file
/// (le_device_db_tlv.c), as in v1; the others are built from patched copies
/// ([`patched_sm`], [`patched_le_device_db`], [`patched_att_server`])
const BLE_EXCLUDE: &[&str] = &["le_device_db_memory.c", "sm.c", "le_device_db_tlv.c", "att_server.c"];

/// Bluedroid SBC encoder (Apache-2.0): mSBC for the headset microphone
/// ([`add_voice_codecs`]), and the reference of feature "sbc-reference"
const SBC_ENCODER: &[&str] = &[
    "sbc_analysis.c",
    "sbc_dct.c",
    "sbc_dct_coeffs.c",
    "sbc_enc_bit_alloc_mono.c",
    "sbc_enc_bit_alloc_ste.c",
    "sbc_enc_coeffs.c",
    "sbc_encoder.c",
    "sbc_packing.c",
];

/// Headset microphone voice codecs (csrc/voice_codecs.c, src/voice.rs):
/// BTstack's Bluedroid SBC glue (instance API, mSBC with H2 sync), the SBC
/// and CVSD packet loss concealment from src/classic, and the Bluedroid SBC
/// decoder (Apache-2.0) from 3rd-party/bluedroid/decoder/srce
const VOICE_CLASSIC: &[&str] = &["btstack_sbc_bluedroid.c", "btstack_sbc_plc.c", "btstack_cvsd_plc.c"];
const SBC_DECODER: &[&str] = &[
    "alloc.c",
    "bitalloc.c",
    "bitalloc-sbc.c",
    "bitstream-decode.c",
    "decoder-oina.c",
    "decoder-private.c",
    "decoder-sbc.c",
    "dequant.c",
    "framing.c",
    "framing-sbc.c",
    "oi_codec_version.c",
    "synthesis-sbc.c",
    "synthesis-dct8.c",
    "synthesis-8-generated.c",
];

/// The voice codec sources and their include paths (the encoder always;
/// feature "sbc-reference" adds only csrc/sbc_reference.c on top)
fn add_voice_codecs(build: &mut cc::Build, btstack: &Path, csrc: &Path) {
    let enc = btstack.join("3rd-party/bluedroid/encoder");
    let dec = btstack.join("3rd-party/bluedroid/decoder");
    build.include(enc.join("include")).include(dec.join("include"));
    build.files(SBC_ENCODER.iter().map(|f| enc.join("srce").join(f)));
    build.files(SBC_DECODER.iter().map(|f| dec.join("srce").join(f)));
    build.files(VOICE_CLASSIC.iter().map(|f| btstack.join("src/classic").join(f)));
    build.file(csrc.join("voice_codecs.c"));
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let btstack =
        env::var_os("BTSTACK_DIR").map(PathBuf::from).unwrap_or_else(|| manifest.join("../../../../extern/btstack"));
    // std::path::absolute, not canonicalize: MSVC does not take \\?\ paths
    let btstack = std::path::absolute(&btstack).unwrap();
    assert!(
        btstack.join("src/hci.c").exists(),
        "BTstack not found at {} (git submodule update --init extern/btstack)",
        btstack.display()
    );
    extern_version::check(&manifest, "btstack", &btstack);
    let csrc = manifest.join("csrc");

    let mut build = cc::Build::new();
    build
        .include(&csrc)
        .include(btstack.join("src"))
        .include(btstack.join("platform/windows"))
        .include(btstack.join("platform/posix"))
        .include(btstack.join("chipset/realtek"))
        .include(btstack.join("chipset/bcm"))
        .include(btstack.join("3rd-party/tinydir"))
        .include(btstack.join("3rd-party/rijndael"))
        .include(btstack.join("3rd-party/micro-ecc"))
        .define("_WIN32_WINNT", "0x0A00")
        .define("NOMINMAX", None)
        .define("WIN32_LEAN_AND_MEAN", None)
        .define("_CRT_SECURE_NO_WARNINGS", None)
        .warnings(false)
        .flag_if_supported("/utf-8")
        .flag_if_supported("/W1")
        // A function called without a declaration (C4013) links against
        // nothing when it is a static inline of a header: an error
        .flag_if_supported("/we4013");

    // The Realtek driver prints why it loads which firmware ("Realtek:
    // Using firmware ...", "Firmware already exists") with printf, to a
    // stdout the resident process does not have: BTstack's
    // ENABLE_PRINTF_TO_LOG sends those lines to the HCI dump log instead
    // (HCI_DUMP_LOG_LEVEL_PRINT), for this file only
    let mut realtek = build.clone();
    // csrc/realtek.c includes btstack_chipset_realtek.c (reads its firmware
    // table), as patched_realtek writes it to OUT_DIR
    let patched = patched_realtek(&btstack);
    realtek.include(patched.parent().unwrap()).define("ENABLE_PRINTF_TO_LOG", None).file(csrc.join("realtek.c"));
    realtek.compile("btstack_realtek");

    let mut core: Vec<PathBuf> = std::fs::read_dir(btstack.join("src"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "c"))
        .filter(|p| !CORE_EXCLUDE.contains(&p.file_name().unwrap().to_str().unwrap()))
        .map(|p| if p.file_name().is_some_and(|n| n == "hci.c") { patched_hci(&btstack) } else { p })
        .collect();
    core.sort();
    build.files(&core);
    let mut ble: Vec<PathBuf> = std::fs::read_dir(btstack.join("src/ble"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "c"))
        .filter(|p| !BLE_EXCLUDE.contains(&p.file_name().unwrap().to_str().unwrap()))
        .collect();
    ble.sort();
    build.files(&ble);
    build.file(patched_sm(&btstack));
    build.file(patched_le_device_db(&btstack));
    build.file(patched_att_server(&btstack));
    build.files(CLASSIC.iter().map(|f| match *f {
        "a2dp.c" => patched_a2dp(&btstack),
        "avdtp_initiator.c" => patched_avdtp_initiator(&btstack),
        "avrcp_target.c" => patched_avrcp_target(&btstack),
        "hfp_ag.c" => patched_hfp_ag(&btstack),
        f => btstack.join("src/classic").join(f),
    }));
    build.file(btstack.join("platform/windows/btstack_run_loop_windows.c"));
    // Software P-256 for LE Secure Connections (ENABLE_MICRO_ECC_P256)
    build.file(btstack.join("3rd-party/micro-ecc/uECC.c"));
    build.file(btstack.join("chipset/bcm/btstack_chipset_bcm.c"));
    build.file(btstack.join("3rd-party/rijndael/rijndael.c"));
    build.file(csrc.join("shim.c"));

    if env::var_os("CARGO_FEATURE_SBC_REFERENCE").is_some() {
        // The encoder sources come with the voice codecs below
        build.file(csrc.join("sbc_reference.c"));
        build.define("BLUECASTER_SBC_REFERENCE", None);
    }

    add_voice_codecs(&mut build, &btstack, &csrc);

    build.compile("btstack");

    for lib in ["setupapi", "winusb", "ws2_32", "advapi32", "user32"] {
        println!("cargo:rustc-link-lib={lib}");
    }
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-env-changed=BTSTACK_DIR");
    rerun_dir(&btstack.join("src"));
    rerun_dir(&btstack.join("src/ble"));
    // For dependents (e.g. tests that need the include paths)
    println!("cargo:root={}", btstack.display());
}

/// src/classic/a2dp.c with its SEP discovery bounded, written to OUT_DIR.
///
/// BTstack 1.8.1 keeps the free sink SEPs a headset lists in a static array
/// of AVDTP_MAX_SEP_NUM (10) without a bounds check: an 11th overwrites the
/// statics after it (the HFP callbacks; BlueCaster then calls a NULL
/// hfp_ag_callback). Upstream fixed it in fc208fcce0 ("a2dp: ignore
/// additional stream endpoints", 2026-07-22, not in a release) by ignoring
/// SEPs beyond the array; that check is added here, and the array holds 62
/// SEPs, every SEID a remote can list (0x01 - 0x3E), so no codec of a
/// headset with many endpoints is ignored.
fn patched_a2dp(btstack: &Path) -> PathBuf {
    fn replace_once(source: &mut String, from: &str, to: &str) {
        assert_eq!(source.matches(from).count(), 1, "a2dp.c patch: `{from}` not found once (BTstack changed?)");
        *source = source.replacen(from, to, 1);
    }
    let mut source = std::fs::read_to_string(btstack.join("src/classic/a2dp.c")).unwrap();
    // Built from OUT_DIR: the header next to it by its path under src/
    replace_once(&mut source, "#include \"a2dp.h\"", "#include \"classic/a2dp.h\"");
    replace_once(&mut source, "#define AVDTP_MAX_SEP_NUM 10", "#define AVDTP_MAX_SEP_NUM 62");
    if !source.contains("discovery limit reached") {
        let store = "a2dp_config_process_sep_discovery_seps[a2dp_config_process_sep_discovery_count++] = sep;";
        replace_once(
            &mut source,
            store,
            &format!(
                "if (a2dp_config_process_sep_discovery_count == AVDTP_MAX_SEP_NUM) {{\n\
                 log_info(\"A2DP: ignoring SEP %u, discovery limit reached\", sep.seid);\n\
                 break;\n\
                 }}\n\
                 {store}"
            ),
        );
    }
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("a2dp.c");
    std::fs::write(&path, source).unwrap();
    path
}

/// src/hci.c keeping a CIS that went down while its ISO data path was set
/// up down, written to OUT_DIR.
///
/// BTstack 1.8.3 marks a CIS of a CIG ACTIVE when the controller confirms
/// its LE Setup ISO Data Path, whatever happened in between. When the ACL
/// dropped right after the first CIS came up (a tester's Realtek adapter),
/// the CIS's Disconnection Complete arrived before that confirmation: the
/// CIS was ACTIVE again, gap_cig_remove sent it an HCI Disconnect (Command
/// Disallowed), waited for its disconnection forever and never removed the
/// CIG. A CIS found IDLE there (its disconnection set that) stays IDLE, and
/// its CIS created event carries Connection Failed to be Established.
///
/// A CIG whose CIS were never created (an earbud that rejected Enable after
/// Set CIG Parameters) was not removed either: gap_cig_remove took their
/// REQUESTED state for a live CIS and waited forever. The CIG stayed in the
/// controller, and after a few such tries Set CIG Parameters failed with
/// Connection Limit Exceeded (0x09). A REQUESTED CIS is discarded like an
/// IDLE one, so LE Remove CIG goes out.
fn patched_hci(btstack: &Path) -> PathBuf {
    const FILE: &str = "hci.c";
    let mut source = std::fs::read_to_string(btstack.join("src").join(FILE)).unwrap().replace("\r\n", "\n");
    let proto = "static void hci_cis_handle_created(hci_iso_stream_t * iso_stream, uint8_t status);\n";
    replace_once(
        &mut source,
        FILE,
        proto,
        &format!(
            "{proto}static uint16_t hci_setup_cis_created(uint8_t * event, hci_iso_stream_t * iso_stream, uint8_t \
             status);\n"
        ),
    );
    replace_once(
        &mut source,
        FILE,
        "                                if ((iso_stream->group_id == cig->cig_id) && (iso_stream->stream_id == cis_id)){\n\
         \x20                                   iso_stream->state = HCI_ISO_STREAM_STATE_ACTIVE;\n\
         \x20                                   hci_cis_handle_created(iso_stream, status);\n",
        "                                if ((iso_stream->group_id == cig->cig_id) && (iso_stream->stream_id == cis_id)\n\
         \x20                                   && (iso_stream->state == HCI_ISO_STREAM_STATE_IDLE)){\n\
         \x20                                   /* BlueCaster: patched by btstack-sys build.rs (patched_hci) */\n\
         \x20                                   uint8_t bluecaster_event[17];\n\
         \x20                                   uint16_t bluecaster_pos = hci_setup_cis_created(bluecaster_event, iso_stream,\n\
         \x20                                       ERROR_CODE_CONNECTION_FAILED_TO_BE_ESTABLISHED);\n\
         \x20                                   hci_emit_btstack_event(bluecaster_event, bluecaster_pos, 0);\n\
         \x20                               } else if ((iso_stream->group_id == cig->cig_id) && (iso_stream->stream_id == cis_id)){\n\
         \x20                                   iso_stream->state = HCI_ISO_STREAM_STATE_ACTIVE;\n\
         \x20                                   hci_cis_handle_created(iso_stream, status);\n",
    );
    let idle = "                            case HCI_ISO_STREAM_STATE_IDLE:\n\
                \x20                               // Either never established or already disconnected, discard\n";
    replace_once(
        &mut source,
        FILE,
        idle,
        &format!(
            "{idle}                            case HCI_ISO_STREAM_STATE_REQUESTED: /* BlueCaster: patched by btstack-sys \
             build.rs (patched_hci), never created */\n"
        ),
    );
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join(FILE);
    std::fs::write(&path, source).unwrap();
    path
}

fn replace_once(source: &mut String, file: &str, from: &str, to: &str) {
    replace_n(source, file, from, to, 1);
}

/// chipset/realtek/btstack_chipset_realtek.c taking an "RTBTCore" (v2)
/// firmware over USB on a chip whose key ID reads 0, written to OUT_DIR as
/// btstack_chipset_realtek_patched.c (csrc/realtek.c includes it).
///
/// BTstack stops such a download over USB ("Wrong key id"), although its
/// own section parser handles key ID 0: it skips the security sections and
/// takes the plain and dummy ones. Realtek's Windows driver treats key ID 0
/// the same way (bt-host realtek_btnic: section 3 only for another key ID).
/// A file with no section for the chip still fails, as "firmware_size == 0".
fn patched_realtek(btstack: &Path) -> PathBuf {
    const FILE: &str = "btstack_chipset_realtek.c";
    let mut source = std::fs::read_to_string(btstack.join("chipset/realtek").join(FILE)).unwrap();
    source.insert_str(0, "/* BlueCaster: patched by btstack-sys build.rs (patched_realtek) */\n");
    replace_once(
        &mut source,
        FILE,
        "if ((realtek_interface == REALTEK_INTERFACE_USB) && (key_id == 0)) {",
        "if (0) { /* BlueCaster: key ID 0, a chip without a security key */",
    );
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("btstack_chipset_realtek_patched.c");
    std::fs::write(&path, source).unwrap();
    println!("cargo:rerun-if-changed={}", btstack.join("chipset/realtek").join(FILE).display());
    path
}

fn replace_n(source: &mut String, file: &str, from: &str, to: &str, n: usize) {
    assert_eq!(source.matches(from).count(), n, "{file} patch: `{from}` not found {n} time(s) (BTstack changed?)");
    *source = source.replace(from, to);
}

/// src/classic/avdtp_initiator.c taking only the response to the command it
/// sent, written to OUT_DIR.
///
/// BTstack 1.8.1 takes any response on the signaling channel as the answer
/// to its pending command, by neither signal nor label. A MediaTek headset
/// on a new link answers our OPEN with a REJECT of DISCOVER under the OPEN's
/// label first, then the OPEN accept: BTstack ended its wait on the reject
/// and dropped the accept ("initiator_config_state is in wrong state"), so
/// the media channel never opened and the stream setup timed out. The signal
/// of the command last sent on each signaling channel is kept here, and a
/// response for another signal is logged and ignored.
fn patched_avdtp_initiator(btstack: &Path) -> PathBuf {
    const FILE: &str = "avdtp_initiator.c";
    // LF patterns: a checkout with core.autocrlf (CI) has CRLF
    let mut source = std::fs::read_to_string(btstack.join("src/classic").join(FILE)).unwrap().replace("\r\n", "\n");
    replace_once(
        &mut source,
        FILE,
        "static bool avdtp_initiator_have_bytes(",
        "/* BlueCaster: patched by btstack-sys build.rs (patched_avdtp_initiator) */\n\
         #define BLUECASTER_PENDING_MAX 8\n\
         static struct { uint16_t cid; uint8_t signal; } bluecaster_pending[BLUECASTER_PENDING_MAX];\n\
         static void bluecaster_pending_set(uint16_t cid, uint8_t signal){\n\
         \x20   int slot = -1;\n\
         \x20   for (int i = 0; i < BLUECASTER_PENDING_MAX; i++){\n\
         \x20       if (bluecaster_pending[i].cid == cid){ slot = i; break; }\n\
         \x20       if ((slot < 0) && (bluecaster_pending[i].cid == 0)) slot = i;\n\
         \x20   }\n\
         \x20   if (slot < 0) slot = 0;\n\
         \x20   bluecaster_pending[slot].cid = cid;\n\
         \x20   bluecaster_pending[slot].signal = signal;\n\
         }\n\
         /* The signal of the command last sent on cid, 0 if none */\n\
         static uint8_t bluecaster_pending_get(uint16_t cid){\n\
         \x20   for (int i = 0; i < BLUECASTER_PENDING_MAX; i++){\n\
         \x20       if (bluecaster_pending[i].cid == cid) return bluecaster_pending[i].signal;\n\
         \x20   }\n\
         \x20   return 0;\n\
         }\n\n\
         static bool avdtp_initiator_have_bytes(",
    );
    // The two plain commands, the delay report, and SET_CONFIGURATION /
    // RECONFIGURE (built in initiator_signaling_packet, maybe fragmented)
    replace_n(
        &mut source,
        FILE,
        "command[1] = (uint8_t)identifier;",
        "command[1] = (uint8_t)identifier; bluecaster_pending_set(cid, (uint8_t)identifier);",
        2,
    );
    replace_once(
        &mut source,
        FILE,
        "command[1] = AVDTP_SI_DELAYREPORT;",
        "command[1] = AVDTP_SI_DELAYREPORT; bluecaster_pending_set(cid, AVDTP_SI_DELAYREPORT);",
    );
    replace_n(
        &mut source,
        FILE,
        "l2cap_send_prepared(connection->l2cap_signaling_cid, pos);",
        "bluecaster_pending_set(connection->l2cap_signaling_cid, (uint8_t)connection->initiator_signaling_packet.signal_identifier);\n\
         \x20           l2cap_send_prepared(connection->l2cap_signaling_cid, pos);",
        2,
    );
    // Only single and start packets carry a signal; the header read just
    // overwrote it, so it is put back for the real answer's continuation
    replace_once(
        &mut source,
        FILE,
        "avdtp_signaling_packet_t * signaling_header = &connection->initiator_signaling_packet;\n",
        "avdtp_signaling_packet_t * signaling_header = &connection->initiator_signaling_packet;\n\
         \x20   uint8_t bluecaster_expected = bluecaster_pending_get(connection->l2cap_signaling_cid);\n\
         \x20   if ((bluecaster_expected != 0) &&\n\
         \x20       ((signaling_header->packet_type == AVDTP_SINGLE_PACKET) || (signaling_header->packet_type == AVDTP_START_PACKET)) &&\n\
         \x20       ((uint8_t)signaling_header->signal_identifier != bluecaster_expected)) {\n\
         \x20       log_error(\"ignoring a response to signal %u while waiting for %u\", (unsigned)signaling_header->signal_identifier, (unsigned)bluecaster_expected);\n\
         \x20       signaling_header->signal_identifier = (avdtp_signal_identifier_t)bluecaster_expected;\n\
         \x20       return;\n\
         \x20   }\n",
    );
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join(FILE);
    std::fs::write(&path, source).unwrap();
    path
}

/// src/classic/avrcp_target.c answering PASS THROUGH with the operation byte
/// it received, written to OUT_DIR (issue #420).
///
/// BTstack 1.8.1 (and its develop branch in 2026-11) clears the state flag
/// (0x80, button released) before it builds the response, so a release is
/// answered as a press. A WH-1000XM6 then repeats the release about 50 times
/// a second and sends no new button presses until it reconnects. The AV/C
/// response carries the command's state flag and operation id. The masked id
/// still decides validity and goes to the app event.
fn patched_avrcp_target(btstack: &Path) -> PathBuf {
    const FILE: &str = "avrcp_target.c";
    // LF patterns: a checkout with core.autocrlf (CI) has CRLF
    let mut source = std::fs::read_to_string(btstack.join("src/classic").join(FILE)).unwrap().replace("\r\n", "\n");
    let echoed = "(avrcp_operation_id_t) operation";
    replace_n(
        &mut source,
        FILE,
        "AVRCP_CTYPE_RESPONSE_REJECTED, operation_id, 0, NULL);",
        &format!("AVRCP_CTYPE_RESPONSE_REJECTED, {echoed}, 0, NULL);"),
        2,
    );
    for ctype in ["AVRCP_CTYPE_RESPONSE_ACCEPTED", "AVRCP_CTYPE_RESPONSE_NOT_IMPLEMENTED"] {
        replace_once(
            &mut source,
            FILE,
            &format!("{ctype}, operation_id, operation_data_field_length, operand);"),
            &format!("{ctype}, {echoed}, operation_data_field_length, operand);"),
        );
    }
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join(FILE);
    std::fs::write(&path, source).unwrap();
    path
}

/// src/classic/hfp_ag.c holding a command that arrives before the answer to
/// the previous one went out, written to OUT_DIR.
///
/// BTstack 1.8.3 answers such a command with ERROR ("Received a command while
/// ok pending"). A WH-1000XM4 sends its battery level (AT+IPHONEACCEV) right
/// after AT+XAPL, before our +XAPL and OK, and does not send it again: after
/// about every other connection BlueCaster showed no battery. The bytes from
/// the start of such a command line on are kept and parsed once OK or ERROR
/// for the previous command has been sent. A line that started before (or
/// more than the buffer holds) gets the ERROR as before.
fn patched_hfp_ag(btstack: &Path) -> PathBuf {
    const FILE: &str = "hfp_ag.c";
    // LF patterns: a checkout with core.autocrlf (CI) has CRLF
    let mut source = std::fs::read_to_string(btstack.join("src/classic").join(FILE)).unwrap().replace("\r\n", "\n");
    replace_once(
        &mut source,
        FILE,
        "static void hfp_ag_run_for_context(hfp_connection_t *hfp_connection){\n",
        r#"/* BlueCaster: patched by btstack-sys build.rs (patched_hfp_ag) */
static void hfp_ag_handle_rfcomm_data(hfp_connection_t * hfp_connection, uint8_t *packet, uint16_t size);
#define BLUECASTER_HELD_MAX 256
static uint16_t bluecaster_held_cid;
static uint16_t bluecaster_held_len;
static uint8_t bluecaster_held[BLUECASTER_HELD_MAX];
static bool bluecaster_holding(const hfp_connection_t * hfp_connection){
    return (bluecaster_held_len > 0) && (bluecaster_held_cid == hfp_connection->rfcomm_cid);
}
/* Keeps received bytes for later; false if they don't fit */
static bool bluecaster_hold(hfp_connection_t * hfp_connection, const uint8_t * data, uint16_t len){
    if ((bluecaster_held_len > 0) && (bluecaster_held_cid != hfp_connection->rfcomm_cid)){
        if (get_hfp_connection_context_for_rfcomm_cid(bluecaster_held_cid) != NULL) return false;
        /* their connection is gone */
        bluecaster_held_len = 0;
    }
    if (len > (BLUECASTER_HELD_MAX - bluecaster_held_len)) return false;
    bluecaster_held_cid = hfp_connection->rfcomm_cid;
    memcpy(&bluecaster_held[bluecaster_held_len], data, len);
    bluecaster_held_len += len;
    return true;
}
/* Parses the kept bytes once no OK or ERROR is waiting to be sent */
static void bluecaster_release(hfp_connection_t * hfp_connection){
    if (!bluecaster_holding(hfp_connection)) return;
    if ((hfp_connection->ok_pending > 0) || (hfp_connection->send_error > 0)) return;
    uint8_t data[BLUECASTER_HELD_MAX];
    uint16_t len = bluecaster_held_len;
    memcpy(data, bluecaster_held, len);
    bluecaster_held_len = 0;
    log_info("HFP AG: parsing %u bytes held while a response was pending", len);
    hfp_ag_handle_rfcomm_data(hfp_connection, data, len);
}

static void hfp_ag_run_for_context(hfp_connection_t *hfp_connection){
"#,
    );
    // Kept bytes first, before anything is sent: their answer follows the
    // pending one
    replace_once(
        &mut source,
        FILE,
        "\tif (hfp_connection->rfcomm_cid == 0) return;\n",
        "\tif (hfp_connection->rfcomm_cid == 0) return;\n\n    bluecaster_release(hfp_connection);\n",
    );
    // Before parsing a byte: hold it, and the rest of the packet, if bytes
    // are held already (order), or if it starts a command line while a
    // response is pending
    replace_once(
        &mut source,
        FILE,
        "        hfp_parse(hfp_connection, packet[pos], 0);\n",
        r#"        if (bluecaster_holding(hfp_connection) ||
            (((hfp_connection->ok_pending > 0) || (hfp_connection->send_error > 0)) &&
             (hfp_connection->parser_state == HFP_PARSER_CMD_HEADER) && (hfp_connection->line_size == 0) &&
             !hfp_parser_is_end_of_line(packet[pos]))){
            if (bluecaster_hold(hfp_connection, &packet[pos], size - pos)){
                log_info("HFP AG: holding a command until the pending response is sent");
                return;
            }
        }
        hfp_parse(hfp_connection, packet[pos], 0);
"#,
    );
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join(FILE);
    std::fs::write(&path, source).unwrap();
    path
}

/// src/ble/sm.c with Cross-Transport Key Derivation from BR/EDR (issue #86),
/// written to OUT_DIR.
///
/// ENABLE_CROSS_TRANSPORT_KEY_DERIVATION is defined for this file only: the
/// rest of BTstack (L2CAP's fixed channel mask) stays as before, so a
/// device is never told BlueCaster takes SMP over BR/EDR. Of what the flag
/// brings, BlueCaster uses one direction: after a classic link is encrypted
/// with Secure Connections, BlueCaster as Central asks the device over the
/// BR/EDR security manager channel (CID 7) for its IRK and identity
/// address, and both sides derive the LE LTK from the link key. That runs
/// only while the shim says so (bluecaster_ctkd_enabled: always since issue #86;
/// SM itself runs only with LE Audio on), after a new pairing (BTstack's
/// own trigger) or on a connection of a device with no keys from it yet
/// (bluecaster_ctkd_wanted). A device that does not list CID 7 is not
/// asked; a failure ends SMP on that link only, never the link.
///
/// The other direction, a BR/EDR link key derived from an LE pairing (issue
/// #86: a dual-mode device paired as LE Audio had no link key): LE pairing
/// offers LinkKey and CT2 (BTstack's own key distribution and AuthReq with
/// the flag), but SM derives the key only when the device agreed, i.e. the
/// Pairing Request and Response both carry LinkKey in the initiator and the
/// responder key distribution (Core Vol 3 Part H 3.6.1: only then does
/// either side derive it; unpatched BTstack 1.8.x derives it on every LE
/// Secure Connections bonding). h6, or h7 when both set CT2 (Part H
/// 2.4.2.4), from the full LTK before it is masked (ESR09 E6405). SM then
/// does not store it itself: it hands the outcome to Rust
/// (bluecaster_rs_ctkd_from_le, see src/ctkd.rs), which keeps the key under
/// a public identity address only.
fn patched_sm(btstack: &Path) -> PathBuf {
    const FILE: &str = "sm.c";
    let mut source = std::fs::read_to_string(btstack.join("src/ble/sm.c")).unwrap();
    source.insert_str(
        0,
        "/* BlueCaster: patched by btstack-sys build.rs (patched_sm) */\n\
         #define ENABLE_CROSS_TRANSPORT_KEY_DERIVATION\n\
         int bluecaster_ctkd_enabled(void);\n\
         int bluecaster_ctkd_wanted(const unsigned char *addr);\n\
         void bluecaster_ctkd_note(const unsigned char *addr, int what);\n\
         void bluecaster_ctkd_from_le(int why, int addr_type, const unsigned char *addr, const unsigned char *key,\n\
                                      int key_type);\n\
         static int bluecaster_link_key_agreed(void);\n",
    );
    // Why a link is not asked over, so the Services dialog can say (issue
    // #86): 1 not Secure Connections, 2 the device opened it, 3 no CID 7
    replace_once(
        &mut source,
        FILE,
        "if (sm_conn->sm_connection_encrypted != 2) return;",
        "if (sm_conn->sm_connection_encrypted != 2) { bluecaster_ctkd_note(sm_conn->sm_peer_address, 1); return; }",
    );
    replace_once(
        &mut source,
        FILE,
        "log_info(\"CTKD: SM_BR_EDR_RESPONDER_W4_PAIRING_REQUEST\");",
        "log_info(\"CTKD: SM_BR_EDR_RESPONDER_W4_PAIRING_REQUEST\"); bluecaster_ctkd_note(sm_conn->sm_peer_address, 2);",
    );
    replace_once(
        &mut source,
        FILE,
        "defer = false;",
        "defer = false; bluecaster_ctkd_note(sm_conn->sm_peer_address, 3);",
    );
    replace_once(
        &mut source,
        FILE,
        "hci_dedicated_bonding_defer_disconnect(con_handle, false);",
        "hci_dedicated_bonding_defer_disconnect(con_handle, false); bluecaster_ctkd_note(sm_conn->sm_peer_address, 3);",
    );
    // BTstack 1.8.1 never marks an ask over BR/EDR as a pairing, so its end
    // (refused, timed out, done) sent no SM_EVENT_PAIRING_COMPLETE
    let ask = "connection->sm_engine_state = SM_BR_EDR_INITIATOR_W4_PAIRING_RESPONSE;";
    replace_once(&mut source, FILE, ask, &format!("{ask}\n                sm_pairing_started(connection);"));
    // Only while enabled; off, the state stays SM_BR_EDR_W4_ENCRYPTION_COMPLETE,
    // where a Pairing Request from the device is refused
    let handler =
        "static void sm_event_handle_classic_encryption_event(sm_connection_t * sm_conn, hci_con_handle_t con_handle){";
    replace_once(&mut source, FILE, handler, &format!("{handler}\n    if (!bluecaster_ctkd_enabled()) return;"));
    // Also for a device paired before (BTstack asks only after a pairing)
    replace_once(
        &mut source,
        FILE,
        "} else if (sm_conn->sm_pairing_requested){",
        "} else if (sm_conn->sm_pairing_requested || bluecaster_ctkd_wanted(sm_conn->sm_peer_address)){",
    );
    // A link key from an LE pairing only when both sides agreed to it: the
    // device derives one only then, so only then is ours the same as its
    let from_le = "static bool sm_ctkd_from_le(void) {";
    replace_once(
        &mut source,
        FILE,
        from_le,
        &format!(
            "static int bluecaster_link_key_agreed(void) {{\n\
             \x20   return (sm_pairing_packet_get_initiator_key_distribution(setup->sm_m_preq)\n\
             \x20         & sm_pairing_packet_get_responder_key_distribution(setup->sm_m_preq)\n\
             \x20         & sm_pairing_packet_get_initiator_key_distribution(setup->sm_s_pres)\n\
             \x20         & sm_pairing_packet_get_responder_key_distribution(setup->sm_s_pres)\n\
             \x20         & SM_KEYDIST_LINK_KEY) != 0;\n\
             }}\n\
             {from_le}\n    if (!bluecaster_link_key_agreed()) return false;"
        ),
    );
    // Stored by Rust, not here (BTstack: under the identity address of any
    // type, kept back when the stored key is authenticated and the derived
    // one is not). BTstack 1.8.3 calls this after an LE bonding (both sides
    // bondable) only; sm_init_setup resets sm_link_key_type for each LE
    // pairing, so a key read for a derivation from BR/EDR never comes here.
    // why: 0 derived, 1 no LE Secure Connections, 2 the device did not
    // agree to LinkKey, 3 not derived otherwise. The key goes over in HCI
    // order (least significant octet first; SM's h6 / h7 result is most
    // significant first), as BTstack's own store and link_keys.txt keep it.
    let store = "static void sm_store_classic_bonding_information(sm_connection_t * sm_conn) {";
    replace_once(
        &mut source,
        FILE,
        store,
        &format!(
            "{store}\n\
             \x20   if (sm_conn != NULL) {{\n\
             \x20       link_key_t bluecaster_key;\n\
             \x20       int why = 3;\n\
             \x20       memset(bluecaster_key, 0, sizeof(bluecaster_key));\n\
             \x20       if (setup->sm_link_key_type != INVALID_LINK_KEY) {{\n\
             \x20           why = 0;\n\
             \x20           reverse_128(setup->sm_link_key, bluecaster_key);\n\
             \x20       }} else if (!setup->sm_use_secure_connections) {{\n\
             \x20           why = 1;\n\
             \x20       }} else if (!bluecaster_link_key_agreed()) {{\n\
             \x20           why = 2;\n\
             \x20       }}\n\
             \x20       bluecaster_ctkd_from_le(why, setup->sm_peer_addr_type, setup->sm_peer_address, bluecaster_key,\n\
             \x20                               (int) setup->sm_link_key_type);\n\
             \x20       return;\n\
             \x20   }}"
        ),
    );
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("sm.c");
    std::fs::write(&path, source).unwrap();
    path
}

/// src/ble/le_device_db_tlv.c telling when a new bond takes the place of an
/// old one, written to OUT_DIR.
///
/// With every entry taken, le_device_db_add writes the new bond over the one
/// with the lowest sequence number (the one stored first; using a bond does
/// not renew it) and tells nobody: the old device's CCC values then belong
/// to the new device, and the old device fails to connect ("pairing failed")
/// without a word in the log. Here the old bond is deleted as
/// gap_delete_bonding would, with GAP_SUBEVENT_BONDING_DELETED
/// (bluecaster_le_bond_evicted in shim.c, src/le_bonds.rs). A bond of a
/// device that is connected stays: its link still refers to the entry.
fn patched_le_device_db(btstack: &Path) -> PathBuf {
    const FILE: &str = "le_device_db_tlv.c";
    // LF patterns: a checkout with core.autocrlf (CI) has CRLF
    let mut source = std::fs::read_to_string(btstack.join("src/ble").join(FILE)).unwrap().replace("\r\n", "\n");
    source.insert_str(
        0,
        "/* BlueCaster: patched by btstack-sys build.rs (patched_le_device_db) */\n\
         int bluecaster_le_bond_in_use(int index);\n\
         void bluecaster_le_bond_evicted(int index, int addr_type, const unsigned char *addr);\n",
    );
    replace_once(
        &mut source,
        FILE,
        "if ((index_for_lowest_seq_nr == -1) || (entry.seq_nr < lowest_seq_nr)){",
        "if (((index_for_lowest_seq_nr == -1) || (entry.seq_nr < lowest_seq_nr)) && !bluecaster_le_bond_in_use(i)){",
    );
    replace_once(
        &mut source,
        FILE,
        "        index_to_use = index_for_lowest_seq_nr;
    } else {
        // should not happen
",
        "        index_to_use = index_for_lowest_seq_nr;
        le_device_db_entry_t evicted;
        le_device_db_tlv_fetch(index_for_lowest_seq_nr, &evicted);
        le_device_db_remove(index_for_lowest_seq_nr);
        new_entry = true;
        bluecaster_le_bond_evicted(index_for_lowest_seq_nr, evicted.addr_type, evicted.addr);
    } else {
        log_error(\"LE bond not stored: every bond belongs to a connected device\");
",
    );
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join(FILE);
    std::fs::write(&path, source).unwrap();
    path
}

/// src/ble/att_server.c with room for more than 256 CCC values (its TLV tag
/// has one byte for the slot), written to OUT_DIR. The first 256 keep their
/// tags (bluecaster_ccc_tag in shim.c).
fn patched_att_server(btstack: &Path) -> PathBuf {
    const FILE: &str = "att_server.c";
    let mut source = std::fs::read_to_string(btstack.join("src/ble").join(FILE)).unwrap().replace("\r\n", "\n");
    replace_once(
        &mut source,
        FILE,
        "static uint32_t att_server_persistent_ccc_tag_for_index(uint8_t index){
    return (((uint8_t)'B') << 24u) | (((uint8_t)'T') << 16u) | (((uint8_t)'C') << 8u) | index;
}
",
        "/* BlueCaster: patched by btstack-sys build.rs (patched_att_server) */
uint32_t bluecaster_ccc_tag(uint32_t slot);
static uint32_t att_server_persistent_ccc_tag_for_index(uint32_t index){
    return bluecaster_ccc_tag(index);
}
",
    );
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join(FILE);
    std::fs::write(&path, source).unwrap();
    path
}

fn rerun_dir(dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
}
