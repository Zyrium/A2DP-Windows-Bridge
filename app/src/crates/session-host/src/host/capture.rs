//! Group `capture`: audio sources run in the agent, their PCM in a channel
//! (agent to core). The core creates each channel's ring, the agent opens
//! it, runs the source ([`audio_capture::CaptureWorker`]) and writes into
//! it; `audio_capture`'s pump reads it in the core. Any number of channels
//! can be open at once (the stream's capture, the microphone uplink).
//!
//! The existing `audio_capture::Capture::open` calls reach this group
//! through [`RemoteCaptureAgent`], installed by
//! [`crate::use_session_agent`].

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use audio_capture::{
    AgentChannel, CAPTURE_RING_BYTES, Capture, CaptureAgent, CaptureError, CaptureOptions, Format, Link, Source,
};
use serde::{Deserialize, Serialize};

use crate::host::devices::capture_error;
use crate::hub::{ChannelState, Hub};
use crate::wire::{decode_reply, to_body};
use crate::{HostError, LocalHost, RemoteHost};

pub const GROUP: &str = "capture";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Opens `source` into the channel `name` (created by the core). Reply:
    /// [`Opened`], or a failure with the `CaptureWireError` as its detail
    Open { channel: u64, name: String, source: Source, options: CaptureOptions },
    /// Starts an opened channel. Reply: null
    Start { channel: u64 },
    /// Stops and closes a channel (sent without waiting). Reply: null
    Close { channel: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opened {
    pub format: Format,
    /// The speakers of the channels (`Capture::channel_mask`)
    pub channel_mask: u32,
}

pub trait CaptureHost {
    /// Opens `source` (where the host runs it); start it as any
    /// [`Capture`]. Blocks until the source is open: not for an audio
    /// thread.
    fn open_capture(&self, source: &Source, options: &CaptureOptions) -> Result<Capture, CaptureError>;
}

impl CaptureHost for LocalHost {
    fn open_capture(&self, source: &Source, options: &CaptureOptions) -> Result<Capture, CaptureError> {
        Capture::open_local(source, options)
    }
}

impl CaptureHost for RemoteHost {
    fn open_capture(&self, source: &Source, options: &CaptureOptions) -> Result<Capture, CaptureError> {
        let (format, channel_mask, channel) = open_remote(self.hub(), source, options)?;
        Ok(Capture::from_agent(format, channel_mask, channel))
    }
}

/// Opens `source` with this process's host
pub fn open_capture(source: &Source, options: &CaptureOptions) -> Result<Capture, CaptureError> {
    crate::host().open_capture(source, options)
}

/// `audio_capture`'s hook: every `Capture::open` goes to the agent
#[derive(Debug)]
pub(crate) struct RemoteCaptureAgent {
    hub: Arc<Hub>,
}

impl RemoteCaptureAgent {
    pub(crate) fn new(hub: Arc<Hub>) -> RemoteCaptureAgent {
        RemoteCaptureAgent { hub }
    }
}

impl CaptureAgent for RemoteCaptureAgent {
    fn open(
        &self,
        source: &Source,
        options: &CaptureOptions,
    ) -> Result<(Format, u32, Box<dyn AgentChannel>), CaptureError> {
        open_remote(&self.hub, source, options)
    }
}

pub(crate) fn open_remote(
    hub: &Arc<Hub>,
    source: &Source,
    options: &CaptureOptions,
) -> Result<(Format, u32, Box<dyn AgentChannel>), CaptureError> {
    // The channel is made for the agent the request goes to (in service
    // mode: handles duplicated into that agent)
    let (reply, agent, id, link) = hub
        .request_with_link(GROUP, CAPTURE_RING_BYTES, |channel, name| {
            to_body(&Request::Open { channel, name, source: source.clone(), options: options.clone() })
        })
        .map_err(capture_error)?;
    let opened: Opened = decode_reply(reply).map_err(capture_error)?;
    let state = Arc::new(ChannelState { generation: agent.generation, lost: Arc::new(AtomicBool::new(false)), link });
    hub.register_channel(&state);
    tracing::debug!("capture agent: channel {id} opened");
    Ok((opened.format, opened.channel_mask, Box::new(RemoteChannel { hub: hub.clone(), id, state })))
}

#[derive(Debug)]
struct RemoteChannel {
    hub: Arc<Hub>,
    id: u64,
    state: Arc<ChannelState>,
}

impl AgentChannel for RemoteChannel {
    fn link(&self) -> Arc<Link> {
        self.state.link.clone()
    }

    fn start(&mut self) -> Result<(), CaptureError> {
        if self.state.lost.load(std::sync::atomic::Ordering::Acquire) {
            return Err(capture_error(HostError::AgentLost));
        }
        let request = Request::Start { channel: self.id };
        let (_, agent) =
            self.hub.request_on(GROUP, to_body(&request), self.hub.request_timeout()).map_err(capture_error)?;
        if agent.generation != self.state.generation {
            // Another agent answered: this channel's agent is gone
            return Err(capture_error(HostError::AgentLost));
        }
        Ok(())
    }

    fn lost_flag(&self) -> Arc<AtomicBool> {
        self.state.lost.clone()
    }
}

impl Drop for RemoteChannel {
    fn drop(&mut self) {
        self.hub.notify(self.state.generation, GROUP, to_body(&Request::Close { channel: self.id }));
    }
}
