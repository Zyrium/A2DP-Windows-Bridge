//! Every AAC packet decodes on its own: a fresh fdk-aac decoder, fed one
//! packet, returns a full frame. A sink that keeps no LATM state between
//! packets (or that joined the stream after the first one) can only play a
//! stream where every AudioMuxElement carries the StreamMuxConfig
//! (AACENC_HEADER_PERIOD = 1, what Android and BlueZ send). With fdk-aac's
//! default period of 10, nine of ten packets fail here.
//!
//! Needs the `decoder` feature: `cargo test -p fdkaac-sys --features decoder`.

use codec::aac::AacEncoder;
use codec::{Encoder, EncoderConfig, Pcm, PeerParams, Quality};
use fdkaac_sys::{Decoder, ffi};
use std::f64::consts::PI;

fn tone(n: usize, ch: usize, sample_rate: u32) -> i16 {
    let hz = if ch == 0 { 1000.0 } else { 1500.0 };
    (0.5 * 32767.0 * (2.0 * PI * hz * n as f64 / f64::from(sample_rate)).sin()).round() as i16
}

#[test]
fn every_packet_decodes_with_a_fresh_decoder() {
    for rate in [44100u32, 48000] {
        for quality in [Quality::High, Quality::Standard, Quality::Mobile] {
            let cfg = EncoderConfig {
                sample_rate: rate,
                channels: 2,
                quality,
                mtu: 1000,
                bits_per_sample: 16,
                abr: false,
                peer: PeerParams::default(),
            };
            let mut enc = AacEncoder::new(&cfg).unwrap();
            let mut out = vec![0u8; enc.max_output_bytes()];
            let mut pcm = vec![0i16; 2048];
            let mut frame = vec![0i16; 2048 * 8];
            let mut failed = Vec::new();
            let calls = 2 * rate as usize / 1024;
            for call in 0..calls {
                for f in 0..1024 {
                    pcm[f * 2] = tone(call * 1024 + f, 0, rate);
                    pcm[f * 2 + 1] = tone(call * 1024 + f, 1, rate);
                }
                let e = enc.encode(Pcm::I16(&pcm), &mut out).unwrap();
                // A sink with no state from earlier packets
                let mut dec = Decoder::open(ffi::TT_MP4_LATM_MCP1, 1).unwrap();
                let ok = dec.fill(&out[..e.bytes]).is_ok() && dec.decode_frame(&mut frame, 0).is_ok();
                if !ok {
                    failed.push(call);
                }
            }
            assert!(
                failed.is_empty(),
                "{rate} Hz {quality:?}: {} of {calls} packets do not decode on their own (first: {:?})",
                failed.len(),
                &failed[..failed.len().min(5)]
            );
        }
    }
}
