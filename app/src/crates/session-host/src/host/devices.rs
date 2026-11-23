//! Group `devices`: Windows' render devices, a device's mix format, the
//! default render device. (Switching the default device, capture devices
//! and endpoint notifications are not here yet.)

use audio_capture::{CaptureError, DeviceInfo, MixFormat};
use serde::{Deserialize, Serialize};

use crate::{LocalHost, RemoteHost};

pub const GROUP: &str = "devices";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Reply: `Result<Vec<DeviceInfo>, CaptureWireError>` as a reply or a
    /// failure with the capture error as its detail
    RenderDevices,
    /// Reply: `Option<MixFormat>`
    MixFormat { device: Option<String> },
    /// Reply: `Option<String>`
    DefaultRenderDevice,
}

/// The facade's device methods. They block until Windows (or the agent)
/// answers: not for the BTstack thread or an audio thread.
pub trait DevicesHost {
    /// Active render endpoints, in Windows' order
    fn render_devices(&self) -> Result<Vec<DeviceInfo>, CaptureError>;
    /// A render device's shared-mode format (None = the default device);
    /// None if it cannot be read
    fn mix_format(&self, device_id: Option<&str>) -> Option<MixFormat>;
    /// The default render device's ID (Console role)
    fn default_render_device(&self) -> Option<String>;
}

impl DevicesHost for LocalHost {
    fn render_devices(&self) -> Result<Vec<DeviceInfo>, CaptureError> {
        audio_capture::render_devices()
    }

    fn mix_format(&self, device_id: Option<&str>) -> Option<MixFormat> {
        audio_capture::mix_format(device_id)
    }

    fn default_render_device(&self) -> Option<String> {
        audio_capture::default_render_device()
    }
}

/// A failed request as a capture error: the agent's own error when it sent
/// one, else what went wrong with the agent
pub(crate) fn capture_error(e: crate::HostError) -> CaptureError {
    match &e {
        crate::HostError::Remote(w) => match w.detail_as::<audio_capture::CaptureWireError>() {
            Some(c) => c.into(),
            None => CaptureError::Agent(e.to_string()),
        },
        _ => CaptureError::Agent(e.to_string()),
    }
}

impl DevicesHost for RemoteHost {
    fn render_devices(&self) -> Result<Vec<DeviceInfo>, CaptureError> {
        self.call(GROUP, &Request::RenderDevices).map_err(capture_error)
    }

    fn mix_format(&self, device_id: Option<&str>) -> Option<MixFormat> {
        let request = Request::MixFormat { device: device_id.map(str::to_string) };
        self.call(GROUP, &request).unwrap_or_else(|e| {
            tracing::warn!("session host: mix format not read: {e}");
            None
        })
    }

    fn default_render_device(&self) -> Option<String> {
        self.call(GROUP, &Request::DefaultRenderDevice).unwrap_or_else(|e| {
            tracing::warn!("session host: default output not read: {e}");
            None
        })
    }
}

/// [`DevicesHost::render_devices`] of this process's host
pub fn render_devices() -> Result<Vec<DeviceInfo>, CaptureError> {
    crate::host().render_devices()
}

/// [`DevicesHost::mix_format`] of this process's host
pub fn mix_format(device_id: Option<&str>) -> Option<MixFormat> {
    crate::host().mix_format(device_id)
}

/// [`DevicesHost::default_render_device`] of this process's host
pub fn default_render_device() -> Option<String> {
    crate::host().default_render_device()
}
