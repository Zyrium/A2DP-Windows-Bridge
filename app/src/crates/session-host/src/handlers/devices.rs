//! Group `devices` in the agent: answers with the direct Windows calls

use serde_json::Value;

use crate::host::devices::{DevicesHost, Request};
use crate::wire::{WireError, decode, reply};
use crate::{AgentContext, Handler, LocalHost};

pub(crate) fn new(_ctx: AgentContext) -> Box<dyn Handler> {
    Box::new(Devices)
}

struct Devices;

impl Handler for Devices {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        match decode(body)? {
            Request::RenderDevices => match LocalHost.render_devices() {
                Ok(list) => reply(list),
                Err(e) => {
                    tracing::warn!("devices: the list failed (HRESULT {:08x?})", e.hresult());
                    Err(WireError::failed("render devices", &audio_capture::CaptureWireError::from(&e)))
                }
            },
            Request::MixFormat { device } => reply(LocalHost.mix_format(device.as_deref())),
            Request::DefaultRenderDevice => reply(LocalHost.default_render_device()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    #[test]
    fn answers_as_the_local_host() {
        let mut h = new(AgentContext::detached());
        let got = h.request(serde_json::json!({"op": "default_render_device"})).unwrap();
        assert_eq!(serde_json::from_value::<Option<String>>(got).unwrap(), LocalHost.default_render_device());
        let got = h.request(serde_json::json!({"op": "mix_format", "device": "no such device"})).unwrap();
        assert_eq!(got, Value::Null);
        match h.request(serde_json::json!({"op": "render_devices"})) {
            Ok(list) => assert_eq!(
                serde_json::from_value::<Vec<audio_capture::DeviceInfo>>(list).unwrap().len(),
                LocalHost.render_devices().unwrap().len()
            ),
            Err(e) => assert_eq!(e.code, ErrorCode::Failed),
        }
        assert_eq!(h.request(serde_json::json!({"op": "nonsense"})).unwrap_err().code, ErrorCode::BadRequest);
    }
}
