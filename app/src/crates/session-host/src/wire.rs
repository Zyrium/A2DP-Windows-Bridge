//! What goes over the agent pipe: length-prefixed JSON frames (the `ipc`
//! crate's framing), the same in both directions.
//!
//! 1. The agent connects and sends [`Frame::Hello`]; the core answers
//!    [`Frame::Welcome`] or [`Frame::Reject`] (another protocol version,
//!    not authorized, or another agent is connected already).
//! 2. Either side sends [`Frame::Request`]s, each with its own id (counted
//!    by the sender) and a group name; the other side answers each with a
//!    [`Frame::Reply`] of that id. The core's requests go to the agent's
//!    group handlers; the agent's requests go to what the core serves
//!    ([`crate::Hub::serve`]).
//! 3. Either side sends [`Frame::Event`]s at any time: no reply, numbered
//!    in order by the sender. The agent's log lines are events of the group
//!    [`LOG_GROUP`].
//! 4. [`Frame::Bye`] from the core tells the agent to exit.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Changes when a frame or a group's messages change incompatibly
pub const PROTOCOL_VERSION: u32 = 1;

/// The agent's log lines (events): [`LogLine`]
pub const LOG_GROUP: &str = "log";

/// Who connected (the agent's first frame)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// The agent's process ID (checked against the pipe's client)
    pub pid: u32,
    /// The Windows session the agent runs in
    pub session_id: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    Hello(Hello),
    Welcome { protocol: u32 },
    Reject { reason: RejectReason },
    Request { id: u64, group: String, body: Value },
    Reply { id: u64, result: Result<Value, WireError> },
    Event { seq: u64, group: String, body: Value },
    Bye,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// Another protocol version (the two programs are from different builds)
    Protocol,
    /// The agent is not the process it says, or not allowed
    Unauthorized,
    /// Another agent is connected
    Busy,
}

/// An error a request's handler returned
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct WireError {
    pub code: ErrorCode,
    /// For the log: counts and states only, no addresses, IDs or paths
    pub message: String,
    /// The group's own error, when it has one (e.g. the capture error)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No handler for the group (the other side is older or newer)
    UnknownGroup,
    /// The body is not a request of the group
    BadRequest,
    /// The group's handler is still busy with too many requests
    Busy,
    /// The request failed (see `detail`)
    Failed,
}

impl WireError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> WireError {
        WireError { code, message: message.into(), detail: None }
    }

    /// A failure with the group's own error as `detail`
    pub fn failed<T: Serialize>(message: impl Into<String>, detail: &T) -> WireError {
        WireError { code: ErrorCode::Failed, message: message.into(), detail: serde_json::to_value(detail).ok() }
    }

    /// The group's own error, if `detail` holds one of type `T`
    pub fn detail_as<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        self.detail.clone().and_then(|d| serde_json::from_value(d).ok())
    }
}

/// One log line of the agent
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    /// "ERROR", "WARN", "INFO", "DEBUG" or "TRACE"
    pub level: String,
    pub message: String,
}

/// Why a request got no answer
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum HostError {
    /// The agent went away (exited, crashed or closed the pipe) before it
    /// answered. Requests sent later go to the next agent.
    #[error("the session agent was lost")]
    AgentLost,
    /// No agent connected in time
    #[error("no session agent is connected")]
    NoAgent,
    /// The agent did not answer in time (it may still be working on it)
    #[error("the session agent did not answer in time")]
    Timeout,
    /// The agent's handler returned an error
    #[error("session agent: {0}")]
    Remote(WireError),
    /// A reply that does not decode, or a frame out of place
    #[error("session agent protocol: {0}")]
    Protocol(String),
}

/// Encodes a request or reply body
pub(crate) fn to_body<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Decodes a request body
pub fn decode<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, WireError> {
    serde_json::from_value(body).map_err(|e| WireError::new(ErrorCode::BadRequest, e.to_string()))
}

/// Decodes a reply
pub(crate) fn decode_reply<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, HostError> {
    serde_json::from_value(body).map_err(|e| HostError::Protocol(format!("bad reply: {e}")))
}

/// A handler's success, encoded
pub fn reply<T: Serialize>(value: T) -> Result<Value, WireError> {
    serde_json::to_value(value).map_err(|e| WireError::new(ErrorCode::Failed, format!("reply: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_have_a_stable_shape() {
        let f = Frame::Request { id: 7, group: "devices".into(), body: serde_json::json!({"op": "render_devices"}) };
        assert_eq!(
            serde_json::to_string(&f).unwrap(),
            r#"{"t":"request","id":7,"group":"devices","body":{"op":"render_devices"}}"#
        );
        let hello = Frame::Hello(Hello { protocol: 1, pid: 8, session_id: 1 });
        assert_eq!(serde_json::to_string(&hello).unwrap(), r#"{"t":"hello","protocol":1,"pid":8,"session_id":1}"#);
        let err = Frame::Reply { id: 7, result: Err(WireError::new(ErrorCode::UnknownGroup, "x")) };
        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(json, r#"{"t":"reply","id":7,"result":{"Err":{"code":"unknown_group","message":"x"}}}"#);
        for f in [hello, err, Frame::Bye, Frame::Reject { reason: RejectReason::Busy }] {
            let back: Frame = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
            assert_eq!(back, f);
        }
    }

    #[test]
    fn details_round_trip() {
        let e = WireError::failed("open", &audio_capture::CaptureWireError::NoDevice);
        let back: Frame =
            serde_json::from_str(&serde_json::to_string(&Frame::Reply { id: 1, result: Err(e.clone()) }).unwrap())
                .unwrap();
        let Frame::Reply { result: Err(got), .. } = back else { panic!() };
        assert_eq!(got.detail_as::<audio_capture::CaptureWireError>(), Some(audio_capture::CaptureWireError::NoDevice));
        assert_eq!(decode::<u32>(serde_json::json!("x")).unwrap_err().code, ErrorCode::BadRequest);
    }
}
