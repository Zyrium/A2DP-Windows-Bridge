//! The input helper's pipe protocol: small binary frames, every field
//! checked. The helper runs as SYSTEM in the user's session, so what it
//! accepts is the most sensitive interface of BlueCaster: no paths, no
//! strings, no flags, only key, button, move and wheel records of a fixed
//! size, with bounded counts and values. A frame that breaks any rule ends
//! the connection (and the helper, which releases what it holds first).
//!
//! A frame is a little-endian `u16` body length, then the body: a kind
//! byte and the kind's fields. Core to helper: [`ToHelper`]; helper to
//! core: [`ToCore`].

use win_sys::input_inject::{Inject, MouseButton};

/// Raised whenever a frame changes meaning
pub const VERSION: u16 = 2;

/// At most this many records in one [`ToHelper::Inject`]
pub const MAX_RECORDS: usize = 64;
/// Bytes per record
const RECORD: usize = 12;
/// The largest body of any frame
pub const MAX_BODY: usize = 2 + MAX_RECORDS * RECORD;

/// The largest pointer move per axis in one record; larger moves are split
/// or clamped by the core
pub const MAX_MOVE: i32 = 1 << 16;
/// The largest wheel turn in one record (WHEEL_DELTA units)
pub const MAX_WHEEL: i32 = 120 * 1024;

const K_INJECT: u8 = 0x01;
const K_RELEASE_ALL: u8 = 0x02;
const K_STOP: u8 = 0x03;
const K_HELLO: u8 = 0x81;
const K_ACK: u8 = 0x82;
const K_LOCK_KEYS: u8 = 0x83;
const K_SAS: u8 = 0x84;
const K_DESKTOP: u8 = 0x85;
const K_SEEN: u8 = 0x86;

const T_KEY: u8 = 1;
const T_VK: u8 = 2;
const T_BUTTON: u8 = 3;
const T_MOVE: u8 = 4;
const T_WHEEL: u8 = 5;
const T_HWHEEL: u8 = 6;
const F_UP: u8 = 1;
const F_EXTENDED: u8 = 2;

/// What the core sends the helper
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToHelper {
    /// Inputs for Windows, in order (1 to [`MAX_RECORDS`])
    Inject(Vec<Inject>),
    /// Let go of everything the helper holds
    ReleaseAll,
    /// Let go of everything and exit
    Stop,
}

/// Which desktop the helper passes input to (a state for the core's log)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopState {
    /// The user's desktop
    Default,
    /// Another one: sign-in or lock screen, UAC prompt
    Secure,
    /// The input desktop cannot be opened: input is refused by Windows
    Unavailable,
}

/// What the helper sends the core
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToCore {
    /// The first frame: protocol version and the helper's process id
    Hello { version: u16, pid: u32 },
    /// Every item up to `processed` (counted from the connection's start,
    /// each record and each [`ToHelper::ReleaseAll`] one item) is done;
    /// `injected` and `blocked`: Windows took or refused this many since
    /// the last ack
    Ack { processed: u64, injected: u32, blocked: u32 },
    /// Windows' Num, Caps and Scroll Lock state changed (mask of
    /// hid_input::leds)
    LockKeys(u8),
    /// Ctrl+Alt+Del was pressed: the core may generate the Secure
    /// Attention Sequence, which `SendInput` cannot
    Sas,
    /// The helper follows another desktop now
    Desktop(DesktopState),
    /// Windows registered input in the helper's session (GetLastInputInfo)
    /// after every item up to `processed` was injected: a hint, sent only
    /// by a helper that really injects
    Seen { processed: u64 },
}

/// Why a frame was refused
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    /// Length 0 or more than [`MAX_BODY`]
    Size,
    /// A kind that does not exist (or goes the other way)
    Kind,
    /// The body's length does not match its kind
    Length,
    /// A record with an unknown type, flag, code or a value out of bounds
    Record,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ProtocolError::Size => "frame size out of bounds",
            ProtocolError::Kind => "unknown frame kind",
            ProtocolError::Length => "frame length does not match its kind",
            ProtocolError::Record => "invalid input record",
        })
    }
}

fn button_code(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 3,
        MouseButton::X1 => 4,
        MouseButton::X2 => 5,
    }
}

fn button_of(code: u16) -> Option<MouseButton> {
    Some(match code {
        1 => MouseButton::Left,
        2 => MouseButton::Right,
        3 => MouseButton::Middle,
        4 => MouseButton::X1,
        5 => MouseButton::X2,
        _ => return None,
    })
}

/// Whether the core may send `i` as it is (else it is clamped first,
/// [`clamp`])
pub fn valid(i: &Inject) -> bool {
    match *i {
        Inject::Key { scan, .. } => (1..=0xFF).contains(&scan),
        Inject::VirtualKey { vk, .. } => (1..=0xFE).contains(&vk),
        Inject::Button { .. } => true,
        Inject::Move { dx, dy } => dx.unsigned_abs() <= MAX_MOVE as u32 && dy.unsigned_abs() <= MAX_MOVE as u32,
        Inject::Wheel(d) | Inject::HWheel(d) => d.unsigned_abs() <= MAX_WHEEL as u32,
    }
}

/// `i` within the bounds: moves and wheel turns clamped; None for a key
/// no keyboard has (it would be refused)
pub fn clamp(i: Inject) -> Option<Inject> {
    let c = |v: i32, m: i32| v.clamp(-m, m);
    let out = match i {
        Inject::Move { dx, dy } => Inject::Move { dx: c(dx, MAX_MOVE), dy: c(dy, MAX_MOVE) },
        Inject::Wheel(d) => Inject::Wheel(c(d, MAX_WHEEL)),
        Inject::HWheel(d) => Inject::HWheel(c(d, MAX_WHEEL)),
        other => other,
    };
    valid(&out).then_some(out)
}

fn record(i: &Inject, out: &mut Vec<u8>) {
    let (t, flags, code, a, b) = match *i {
        Inject::Key { scan, extended, up } => {
            (T_KEY, (u8::from(up) * F_UP) | (u8::from(extended) * F_EXTENDED), scan, 0, 0)
        }
        Inject::VirtualKey { vk, up } => (T_VK, u8::from(up) * F_UP, vk, 0, 0),
        Inject::Button { button, up } => (T_BUTTON, u8::from(up) * F_UP, button_code(button), 0, 0),
        Inject::Move { dx, dy } => (T_MOVE, 0, 0, dx, dy),
        Inject::Wheel(d) => (T_WHEEL, 0, 0, d, 0),
        Inject::HWheel(d) => (T_HWHEEL, 0, 0, d, 0),
    };
    out.extend([t, flags]);
    out.extend(code.to_le_bytes());
    out.extend(a.to_le_bytes());
    out.extend(b.to_le_bytes());
}

fn parse_record(r: &[u8; RECORD]) -> Result<Inject, ProtocolError> {
    let bad = Err(ProtocolError::Record);
    let (t, flags, code) = (r[0], r[1], u16::from_le_bytes([r[2], r[3]]));
    let a = i32::from_le_bytes([r[4], r[5], r[6], r[7]]);
    let b = i32::from_le_bytes([r[8], r[9], r[10], r[11]]);
    let up = flags & F_UP != 0;
    let i = match t {
        T_KEY if flags & !(F_UP | F_EXTENDED) == 0 && a == 0 && b == 0 => {
            Inject::Key { scan: code, extended: flags & F_EXTENDED != 0, up }
        }
        T_VK if flags & !F_UP == 0 && a == 0 && b == 0 => Inject::VirtualKey { vk: code, up },
        T_BUTTON if flags & !F_UP == 0 && a == 0 && b == 0 => match button_of(code) {
            Some(button) => Inject::Button { button, up },
            None => return bad,
        },
        T_MOVE if flags == 0 && code == 0 => Inject::Move { dx: a, dy: b },
        T_WHEEL if flags == 0 && code == 0 && b == 0 => Inject::Wheel(a),
        T_HWHEEL if flags == 0 && code == 0 && b == 0 => Inject::HWheel(a),
        _ => return bad,
    };
    if valid(&i) { Ok(i) } else { bad }
}

/// The frame (length and body) of a message to the helper. Panics on an
/// inject batch out of bounds (a bug in the core: the queue makes batches).
pub fn encode_to_helper(m: &ToHelper) -> Vec<u8> {
    let mut body = Vec::with_capacity(MAX_BODY);
    match m {
        ToHelper::Inject(list) => {
            assert!((1..=MAX_RECORDS).contains(&list.len()) && list.iter().all(valid), "inject batch out of bounds");
            body.extend([K_INJECT, list.len() as u8]);
            for i in list {
                record(i, &mut body);
            }
        }
        ToHelper::ReleaseAll => body.push(K_RELEASE_ALL),
        ToHelper::Stop => body.push(K_STOP),
    }
    frame(body)
}

fn frame(body: Vec<u8>) -> Vec<u8> {
    let mut out = (body.len() as u16).to_le_bytes().to_vec();
    out.extend(body);
    out
}

/// A body from the core, checked
pub fn decode_to_helper(body: &[u8]) -> Result<ToHelper, ProtocolError> {
    if body.is_empty() || body.len() > MAX_BODY {
        return Err(ProtocolError::Size);
    }
    match body[0] {
        K_INJECT => {
            let n = *body.get(1).ok_or(ProtocolError::Length)? as usize;
            if n == 0 || n > MAX_RECORDS {
                return Err(ProtocolError::Record);
            }
            if body.len() != 2 + n * RECORD {
                return Err(ProtocolError::Length);
            }
            body[2..].as_chunks::<RECORD>().0.iter().map(parse_record).collect::<Result<_, _>>().map(ToHelper::Inject)
        }
        K_RELEASE_ALL | K_STOP if body.len() != 1 => Err(ProtocolError::Length),
        K_RELEASE_ALL => Ok(ToHelper::ReleaseAll),
        K_STOP => Ok(ToHelper::Stop),
        _ => Err(ProtocolError::Kind),
    }
}

/// The frame of a message to the core
pub fn encode_to_core(m: &ToCore) -> Vec<u8> {
    let mut body = Vec::with_capacity(32);
    match *m {
        ToCore::Hello { version, pid } => {
            body.push(K_HELLO);
            body.extend(version.to_le_bytes());
            body.extend(pid.to_le_bytes());
        }
        ToCore::Ack { processed, injected, blocked } => {
            body.push(K_ACK);
            body.extend(processed.to_le_bytes());
            body.extend(injected.to_le_bytes());
            body.extend(blocked.to_le_bytes());
        }
        ToCore::LockKeys(mask) => body.extend([K_LOCK_KEYS, mask & hid_input::leds::ALL]),
        ToCore::Seen { processed } => {
            body.push(K_SEEN);
            body.extend(processed.to_le_bytes());
        }
        ToCore::Sas => body.push(K_SAS),
        ToCore::Desktop(d) => body.extend([
            K_DESKTOP,
            match d {
                DesktopState::Default => 0,
                DesktopState::Secure => 1,
                DesktopState::Unavailable => 2,
            },
        ]),
    }
    frame(body)
}

/// A body from the helper, checked (the core trusts its helper no more
/// than it must)
pub fn decode_to_core(body: &[u8]) -> Result<ToCore, ProtocolError> {
    if body.is_empty() || body.len() > MAX_BODY {
        return Err(ProtocolError::Size);
    }
    let len = |n: usize| if body.len() == n { Ok(()) } else { Err(ProtocolError::Length) };
    let u32_at = |i: usize| u32::from_le_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]]);
    match body[0] {
        K_HELLO => {
            len(7)?;
            Ok(ToCore::Hello { version: u16::from_le_bytes([body[1], body[2]]), pid: u32_at(3) })
        }
        K_ACK => {
            len(17)?;
            let mut p = [0u8; 8];
            p.copy_from_slice(&body[1..9]);
            Ok(ToCore::Ack { processed: u64::from_le_bytes(p), injected: u32_at(9), blocked: u32_at(13) })
        }
        K_LOCK_KEYS => {
            len(2)?;
            if body[1] & !hid_input::leds::ALL != 0 {
                return Err(ProtocolError::Record);
            }
            Ok(ToCore::LockKeys(body[1]))
        }
        K_SAS => len(1).map(|_| ToCore::Sas),
        K_SEEN => {
            len(9)?;
            let mut p = [0u8; 8];
            p.copy_from_slice(&body[1..9]);
            Ok(ToCore::Seen { processed: u64::from_le_bytes(p) })
        }
        K_DESKTOP => {
            len(2)?;
            Ok(ToCore::Desktop(match body[1] {
                0 => DesktopState::Default,
                1 => DesktopState::Secure,
                2 => DesktopState::Unavailable,
                _ => return Err(ProtocolError::Record),
            }))
        }
        _ => Err(ProtocolError::Kind),
    }
}

/// Reads one frame's body; Ok(None) at a clean end of the stream between
/// frames. A length out of bounds is an error before anything more is read.
pub async fn read_frame<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    use tokio::io::AsyncReadExt;
    let mut len = [0u8; 2];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u16::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_BODY {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, ProtocolError::Size.to_string()));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(frame: &[u8]) -> &[u8] {
        assert_eq!(u16::from_le_bytes([frame[0], frame[1]]) as usize, frame.len() - 2);
        &frame[2..]
    }

    fn every_kind() -> Vec<Inject> {
        vec![
            Inject::Key { scan: 0x1E, extended: false, up: false },
            Inject::Key { scan: 0x1D, extended: true, up: true },
            Inject::VirtualKey { vk: 0xB3, up: false },
            Inject::Button { button: MouseButton::X2, up: true },
            Inject::Move { dx: -MAX_MOVE, dy: 7 },
            Inject::Wheel(-120),
            Inject::HWheel(MAX_WHEEL),
        ]
    }

    #[test]
    fn messages_round_trip() {
        for m in [ToHelper::Inject(every_kind()), ToHelper::ReleaseAll, ToHelper::Stop] {
            assert_eq!(decode_to_helper(body(&encode_to_helper(&m))), Ok(m));
        }
        let full = ToHelper::Inject(vec![Inject::Move { dx: 1, dy: 1 }; MAX_RECORDS]);
        assert_eq!(encode_to_helper(&full).len(), 2 + MAX_BODY);
        for m in [
            ToCore::Hello { version: VERSION, pid: 1234 },
            ToCore::Ack { processed: u64::MAX - 1, injected: 3, blocked: 9 },
            ToCore::LockKeys(5),
            ToCore::Sas,
            ToCore::Desktop(DesktopState::Secure),
            ToCore::Desktop(DesktopState::Unavailable),
            ToCore::Seen { processed: 77 },
        ] {
            assert_eq!(decode_to_core(body(&encode_to_core(&m))), Ok(m));
        }
    }

    #[test]
    fn malformed_and_oversized_frames_are_refused() {
        let good = encode_to_helper(&ToHelper::Inject(vec![Inject::Key { scan: 0x1E, extended: false, up: false }]));
        let b = body(&good).to_vec();
        assert_eq!(decode_to_helper(&[]), Err(ProtocolError::Size));
        assert_eq!(decode_to_helper(&vec![K_INJECT; MAX_BODY + 1]), Err(ProtocolError::Size));
        assert_eq!(decode_to_helper(&[0x7F]), Err(ProtocolError::Kind));
        // A kind of the other direction
        assert_eq!(decode_to_helper(&[K_HELLO, 1, 0, 0, 0, 0, 0]), Err(ProtocolError::Kind));
        assert_eq!(decode_to_helper(&[K_STOP, 0]), Err(ProtocolError::Length));
        assert_eq!(decode_to_helper(&[K_INJECT]), Err(ProtocolError::Length));
        assert_eq!(decode_to_helper(&[K_INJECT, 0]), Err(ProtocolError::Record));
        assert_eq!(decode_to_helper(&[K_INJECT, MAX_RECORDS as u8 + 1]), Err(ProtocolError::Record));
        // Count and length disagree
        let mut short = b.clone();
        short.pop();
        assert_eq!(decode_to_helper(&short), Err(ProtocolError::Length));
        let mut two = b.clone();
        two[1] = 2;
        assert_eq!(decode_to_helper(&two), Err(ProtocolError::Length));
        // Each field of a record
        let bad = |at: usize, v: u8| {
            let mut x = b.clone();
            x[2 + at] = v;
            decode_to_helper(&x)
        };
        assert_eq!(bad(0, 0), Err(ProtocolError::Record), "type 0");
        assert_eq!(bad(0, 9), Err(ProtocolError::Record), "unknown type");
        assert_eq!(bad(1, 4), Err(ProtocolError::Record), "unknown flag");
        assert_eq!(bad(2, 0), Err(ProtocolError::Record), "scan code 0");
        assert_eq!(bad(3, 1), Err(ProtocolError::Record), "scan code above 0xFF");
        assert_eq!(bad(4, 1), Err(ProtocolError::Record), "a value on a key");
        let rec = |t: u8, f: u8, code: u16, a: i32, b: i32| {
            let mut x = vec![K_INJECT, 1, t, f];
            x.extend(code.to_le_bytes());
            x.extend(a.to_le_bytes());
            x.extend(b.to_le_bytes());
            decode_to_helper(&x)
        };
        assert_eq!(rec(T_BUTTON, 0, 6, 0, 0), Err(ProtocolError::Record));
        assert_eq!(rec(T_VK, 0, 0xFF, 0, 0), Err(ProtocolError::Record));
        assert_eq!(rec(T_VK, F_EXTENDED, 0x41, 0, 0), Err(ProtocolError::Record));
        assert_eq!(rec(T_MOVE, 0, 0, MAX_MOVE + 1, 0), Err(ProtocolError::Record));
        assert_eq!(rec(T_MOVE, 0, 0, i32::MIN, 0), Err(ProtocolError::Record));
        assert_eq!(rec(T_MOVE, F_UP, 0, 1, 0), Err(ProtocolError::Record));
        assert_eq!(rec(T_WHEEL, 0, 0, 120, 1), Err(ProtocolError::Record));
        assert_eq!(rec(T_HWHEEL, 0, 0, MAX_WHEEL + 1, 0), Err(ProtocolError::Record));
        assert!(rec(T_MOVE, 0, 0, -5, MAX_MOVE).is_ok());
        // From the helper
        assert_eq!(decode_to_core(&[K_LOCK_KEYS, 8]), Err(ProtocolError::Record));
        assert_eq!(decode_to_core(&[K_DESKTOP, 3]), Err(ProtocolError::Record));
        assert_eq!(decode_to_core(&[K_ACK, 0]), Err(ProtocolError::Length));
        assert_eq!(decode_to_core(&[K_SEEN, 0, 0]), Err(ProtocolError::Length));
        assert_eq!(decode_to_core(&[K_INJECT, 1]), Err(ProtocolError::Kind));
    }

    #[test]
    fn values_are_clamped_and_unknown_keys_dropped() {
        assert_eq!(clamp(Inject::Move { dx: i32::MIN, dy: 3 }), Some(Inject::Move { dx: -MAX_MOVE, dy: 3 }));
        assert_eq!(clamp(Inject::Wheel(i32::MAX)), Some(Inject::Wheel(MAX_WHEEL)));
        assert_eq!(clamp(Inject::Key { scan: 0x100, extended: false, up: true }), None);
        assert_eq!(clamp(Inject::VirtualKey { vk: 0, up: true }), None);
        assert!(every_kind().into_iter().all(|i| clamp(i) == Some(i)));
    }

    #[test]
    fn frames_are_read_with_their_bound() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            let mut two = encode_to_helper(&ToHelper::ReleaseAll);
            two.extend(encode_to_helper(&ToHelper::Stop));
            let mut r = &two[..];
            assert_eq!(read_frame(&mut r).await.unwrap(), Some(vec![K_RELEASE_ALL]));
            assert_eq!(read_frame(&mut r).await.unwrap(), Some(vec![K_STOP]));
            assert_eq!(read_frame(&mut r).await.unwrap(), None);
            // A length above the bound is refused before its body is read
            let mut big = &((MAX_BODY + 1) as u16).to_le_bytes()[..];
            assert!(read_frame(&mut big).await.is_err());
            let mut zero = &[0u8, 0][..];
            assert!(read_frame(&mut zero).await.is_err());
            // Cut inside a body: an error, not a clean end
            let mut cut = &[3u8, 0, 1][..];
            assert!(read_frame(&mut cut).await.is_err());
        });
    }
}
