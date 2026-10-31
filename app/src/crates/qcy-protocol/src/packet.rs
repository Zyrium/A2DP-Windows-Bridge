//! The command framing on characteristics 1001 (commands) and 1002
//! (responses):
//!
//! ```text
//! FF <body length> <opcode> <parameter length> <parameters...>
//! ```
//!
//! The body length counts everything after it (opcode, parameter length and
//! the parameters). A request is opcode `FE` with the queried opcode as its
//! only parameter. A response may hold several opcode / length / parameters
//! blocks one after another.

use std::fmt;

pub const FRAME_START: u8 = 0xFF;
/// Opcode of a request: its parameter is the opcode to query
pub const REQUEST_OPCODE: u8 = 0xFE;
/// The body length is one byte and counts the opcode and the parameter
/// length byte too
pub const MAX_PARAMETERS: usize = 253;

/// One opcode / parameters block of a response
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub opcode: u8,
    pub parameters: Vec<u8>,
}

/// More than [`MAX_PARAMETERS`] parameter bytes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooManyParameters;

impl fmt::Display for TooManyParameters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a QCY command can carry at most {MAX_PARAMETERS} parameter bytes")
    }
}

impl std::error::Error for TooManyParameters {}

/// Frames one command
pub fn pack(opcode: u8, parameters: &[u8]) -> Result<Vec<u8>, TooManyParameters> {
    if parameters.len() > MAX_PARAMETERS {
        return Err(TooManyParameters);
    }
    Ok(frame(opcode, parameters))
}

/// [`pack`] for the typed builders, whose parameters are small by
/// construction
pub(crate) fn frame(opcode: u8, parameters: &[u8]) -> Vec<u8> {
    debug_assert!(parameters.len() <= MAX_PARAMETERS);
    let mut packet = Vec::with_capacity(parameters.len() + 4);
    packet.push(FRAME_START);
    packet.push((parameters.len() + 2) as u8);
    packet.push(opcode);
    packet.push(parameters.len() as u8);
    packet.extend_from_slice(parameters);
    packet
}

/// "FF03FE0117", the form of the transcripts
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// The blocks of a received packet; empty when it is not a well-formed frame
/// (wrong start byte, length mismatch, a block running past the end)
pub fn parse(packet: &[u8]) -> Vec<Command> {
    if packet.len() < 4 || packet[0] != FRAME_START {
        return Vec::new();
    }
    if packet[1] as usize + 2 != packet.len() {
        return Vec::new();
    }
    let mut commands = Vec::new();
    let mut offset = 2;
    while offset < packet.len() {
        if offset + 2 > packet.len() {
            return Vec::new();
        }
        let opcode = packet[offset];
        let length = packet[offset + 1] as usize;
        offset += 2;
        if offset + length > packet.len() {
            return Vec::new();
        }
        commands.push(Command { opcode, parameters: packet[offset..offset + length].to_vec() });
        offset += length;
    }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::hex;

    #[test]
    fn hex_form_of_the_transcripts() {
        assert_eq!(to_hex(&hex("FF03FE0117")), "FF03FE0117");
        assert_eq!(to_hex(&[]), "");
        assert_eq!(to_hex(&[0x0A, 0xB0]), "0AB0");
    }

    #[test]
    fn packs_a_request() {
        // ANC query of the transcript
        assert_eq!(pack(REQUEST_OPCODE, &[0x17]).unwrap(), hex("FF03FE0117"));
    }

    #[test]
    fn parses_a_response() {
        // ANC response of the transcript
        let cmds = parse(&hex("FF051703010302"));
        assert_eq!(cmds, [Command { opcode: 0x17, parameters: vec![1, 3, 2] }]);
    }

    #[test]
    fn parses_several_blocks() {
        let cmds = parse(&hex("FF06 09 01 01 10 01 02"));
        assert_eq!(
            cmds,
            [Command { opcode: 0x09, parameters: vec![1] }, Command { opcode: 0x10, parameters: vec![2] }]
        );
    }

    #[test]
    fn rejects_malformed_frames() {
        assert!(parse(&[]).is_empty());
        // Wrong start byte
        assert!(parse(&hex("FE051703010302")).is_empty());
        // Length mismatch
        assert!(parse(&hex("FF061703010302")).is_empty());
        // A block that runs past the end
        assert!(parse(&hex("FF0517050102 03")).is_empty());
        // Too short
        assert!(parse(&hex("FF0117")).is_empty());
    }

    #[test]
    fn parameter_limit() {
        assert!(pack(0x22, &vec![0; MAX_PARAMETERS]).is_ok());
        assert_eq!(pack(0x22, &vec![0; MAX_PARAMETERS + 1]), Err(TooManyParameters));
    }

    #[test]
    fn pack_then_parse_round_trips() {
        let packet = pack(0x22, &[1, 2, 3, 4]).unwrap();
        assert_eq!(parse(&packet), [Command { opcode: 0x22, parameters: vec![1, 2, 3, 4] }]);
    }
}
