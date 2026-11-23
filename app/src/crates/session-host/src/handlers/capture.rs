//! Group `capture` in the agent: one [`CaptureWorker`] per open channel,
//! all stopped when the connection ends

use std::collections::HashMap;

use audio_capture::{CaptureError, CaptureWireError, CaptureWorker};
use serde_json::Value;

use crate::host::capture::{Opened, Request};
use crate::wire::{ErrorCode, WireError, decode, reply};
use crate::{AgentContext, Handler};

/// Channels one core may have open at once
const MAX_CHANNELS: usize = 16;

pub(crate) fn new(_ctx: AgentContext) -> Box<dyn Handler> {
    Box::new(Captures::default())
}

#[derive(Default)]
struct Captures {
    channels: HashMap<u64, CaptureWorker>,
}

fn failed(what: &str, e: &CaptureError) -> WireError {
    let wire = CaptureWireError::from(e);
    // The variant only: its text can hold a device ID
    let kind = format!("{wire:?}");
    let kind = kind.split(['(', ' ', '{']).next().unwrap_or_default();
    tracing::warn!("capture agent: {what} failed ({kind}, HRESULT {:08x?})", e.hresult());
    WireError::failed(what, &wire)
}

impl Handler for Captures {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        match decode(body)? {
            Request::Open { channel, name, source, options } => {
                if self.channels.len() >= MAX_CHANNELS && !self.channels.contains_key(&channel) {
                    return Err(WireError::new(ErrorCode::Busy, "too many capture channels"));
                }
                // A channel number used again replaces its old source
                self.channels.remove(&channel);
                let (worker, format, channel_mask) =
                    CaptureWorker::open(&name, &source, &options).map_err(|e| failed("open", &e))?;
                self.channels.insert(channel, worker);
                tracing::info!("capture agent: channel opened ({} open)", self.channels.len());
                reply(Opened { format, channel_mask })
            }
            Request::Start { channel } => {
                let worker = self
                    .channels
                    .get_mut(&channel)
                    .ok_or_else(|| WireError::new(ErrorCode::Failed, "no such channel"))?;
                worker.start().map_err(|e| failed("start", &e))?;
                reply(())
            }
            Request::Close { channel } => {
                if self.channels.remove(&channel).is_some() {
                    tracing::info!("capture agent: channel closed ({} open)", self.channels.len());
                }
                reply(())
            }
        }
    }
}

impl Drop for Captures {
    fn drop(&mut self) {
        if !self.channels.is_empty() {
            tracing::info!("capture agent: stopping {} channel(s)", self.channels.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audio_capture::{CaptureOptions, Format, Link, SampleFormat, Source};

    fn open(h: &mut Box<dyn Handler>, channel: u64, name: &str, source: Source) -> Result<Value, WireError> {
        let r = Request::Open { channel, name: name.into(), source, options: CaptureOptions::default() };
        h.request(serde_json::to_value(r).unwrap())
    }

    #[test]
    fn channels_open_start_and_close() {
        let mut h = new(AgentContext::detached());
        let (_, name) = crate::hub::new_channel();
        let link = Link::create(&name, 64 << 10).unwrap();
        let opened: Opened = serde_json::from_value(open(&mut h, 1, &name, Source::TestTone).unwrap()).unwrap();
        assert_eq!(opened.format, Format { sample_rate: 48_000, channels: 2, sample_format: SampleFormat::F32 });
        h.request(serde_json::to_value(Request::Start { channel: 1 }).unwrap()).unwrap();
        // The tone writes into the ring
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while link.stat(pcm_link::Stat::WriterRecords).load(std::sync::atomic::Ordering::Relaxed) == 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let err = h.request(serde_json::to_value(Request::Start { channel: 2 }).unwrap()).unwrap_err();
        assert_eq!(err.message, "no such channel");
        h.request(serde_json::to_value(Request::Close { channel: 1 }).unwrap()).unwrap();
        // Closing twice is fine
        h.request(serde_json::to_value(Request::Close { channel: 1 }).unwrap()).unwrap();
    }

    #[test]
    fn open_errors_carry_the_capture_error() {
        let mut h = new(AgentContext::detached());
        let (_, name) = crate::hub::new_channel();
        let _link = Link::create(&name, 64 << 10).unwrap();
        let err = open(&mut h, 1, &name, Source::RenderDevice { id: String::new() }).unwrap_err();
        assert_eq!(err.code, ErrorCode::Failed);
        assert!(matches!(err.detail_as::<CaptureWireError>(), Some(CaptureWireError::DeviceNotFound(_))));
    }
}
