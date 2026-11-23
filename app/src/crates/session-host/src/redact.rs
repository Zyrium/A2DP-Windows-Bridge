//! The session agent's log lines as they reach the core's log: counts and
//! states only.
//!
//! The agent runs code that also runs in the core's own process (the
//! [`crate::LocalHost`] path), and some of its log lines carry what only the
//! user's own log should hold: a device's or player's name, an endpoint ID,
//! a file path. Forwarded, those lines land in the core's log, which in
//! service mode is the machine's data and may go into a bug report. So each
//! forwarded line is reduced ([`reduce`]): by the agent before it leaves
//! (`LogForwardLayer`), and again by the core with what arrives (the core
//! cannot trust the agent), which also removes control characters (no
//! forged lines) and caps the length.
//!
//! What becomes a placeholder:
//! - quoted text (`"..."`, `\u{201C}...\u{201D}`): `<name>`. A log line that
//!   can run in the agent quotes the names it logs: a name without quotes
//!   cannot be told from the other words.
//! - a word with a backslash or a drive (`C:/`), and the few words between
//!   two such words (a path with spaces): `<path>`
//! - a file name with the extension of a program, library or data file:
//!   `<file>`
//! - an ID in braces (`{0.0.0.00000000}.{...}`) or a GUID: `<id>`
//! - a SID (`S-1-5-...`): `<sid>`
//! - a Bluetooth address: `<address>`
//! - a run of 16 or more hex digits with a letter (a key): `<hex>`

/// The longest line the core takes from the agent (characters)
pub(crate) const MAX_CHARS: usize = 2000;

/// Words between two path words that are taken as part of the path
const PATH_GAP: usize = 3;

/// File extensions whose names are redacted
const FILE_EXTENSIONS: &[&str] =
    &["exe", "dll", "sys", "lnk", "bat", "cmd", "ps1", "msi", "wav", "json", "txt", "log", "dmp", "pklg", "zip"];

/// A forwarded log line without names, IDs, paths and control characters,
/// at most [`MAX_CHARS`] long
pub(crate) fn reduce(message: &str) -> String {
    // Bounded work for a line of any length: what is cut here is cut anyway
    let clean: String = message.chars().take(2 * MAX_CHARS).map(|c| if is_hidden(c) { ' ' } else { c }).collect();
    let mut out = redact_words(&redact_quotes(&clean));
    if let Some((cut, _)) = out.char_indices().nth(MAX_CHARS) {
        out.truncate(cut);
        out.push_str("...");
    }
    out
}

/// Control characters, and the format characters that change how a line
/// reads (bidirectional overrides, zero-width characters)
fn is_hidden(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// Quoted text becomes `<name>`; an unclosed quote takes the rest of the
/// line
fn redact_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        let close = match c {
            '"' => '"',
            '\u{201C}' => '\u{201D}',
            _ => {
                out.push(c);
                continue;
            }
        };
        // Up to the closing quote; in "..." a backslash escapes the next
        // character (Rust's debug format)
        while let Some(n) = chars.next() {
            if n == close {
                break;
            }
            if close == '"' && n == '\\' {
                chars.next();
            }
        }
        out.push_str("<name>");
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Plain,
    Path,
    Other(&'static str),
}

/// One word of the line: its leading and trailing punctuation, its core,
/// and the white space after it
struct Word<'a> {
    lead: &'a str,
    core: &'a str,
    trail: &'a str,
    space: &'a str,
    kind: Kind,
}

const LEAD: &[char] = &['(', '[', '<', '\'', '`', ','];
const TRAIL: &[char] = &[')', ']', '>', ',', ';', ':', '.', '\'', '`', '!', '?'];

fn split_words(s: &str) -> Vec<Word<'_>> {
    let mut words = Vec::new();
    let mut rest = s;
    // White space before the first word
    let first = rest.len() - rest.trim_start().len();
    if first > 0 {
        words.push(Word { lead: "", core: "", trail: "", space: &rest[..first], kind: Kind::Plain });
        rest = &rest[first..];
    }
    while !rest.is_empty() {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, after) = rest.split_at(end);
        let space_len = after.len() - after.trim_start().len();
        let (space, next) = after.split_at(space_len);
        let core_start = word.len() - word.trim_start_matches(LEAD).len();
        let (lead, tail) = word.split_at(core_start);
        let core = tail.trim_end_matches(TRAIL);
        let trail = &tail[core.len()..];
        words.push(Word { lead, core, trail, space, kind: kind(core) });
        rest = next;
    }
    words
}

fn redact_words(s: &str) -> String {
    let mut words = split_words(s);
    // The words between two path words close together: a path with spaces
    let paths: Vec<usize> = words.iter().enumerate().filter(|(_, w)| w.kind == Kind::Path).map(|(i, _)| i).collect();
    for pair in paths.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let between = &words[a + 1..b];
        if b - a <= PATH_GAP + 1
            && words[a].trail.is_empty()
            && between.iter().all(|w| w.kind == Kind::Plain && w.trail.is_empty())
        {
            for w in &mut words[a + 1..b] {
                w.kind = Kind::Path;
            }
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        match w.kind {
            Kind::Plain => {
                out.push_str(w.lead);
                out.push_str(w.core);
                out.push_str(w.trail);
                out.push_str(w.space);
            }
            Kind::Other(placeholder) => {
                out.push_str(w.lead);
                out.push_str(placeholder);
                out.push_str(w.trail);
                out.push_str(w.space);
            }
            Kind::Path => {
                // One placeholder for the whole path
                let mut last = i;
                while last + 1 < words.len() && words[last + 1].kind == Kind::Path && words[last].trail.is_empty() {
                    last += 1;
                }
                out.push_str(w.lead);
                out.push_str("<path>");
                out.push_str(words[last].trail);
                out.push_str(words[last].space);
                i = last;
            }
        }
        i += 1;
    }
    out
}

fn kind(core: &str) -> Kind {
    if core.is_empty() {
        return Kind::Plain;
    }
    let b = core.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/';
    if core.contains('\\') || drive || core.starts_with("file:") {
        return Kind::Path;
    }
    if has_braced_id(core) || has_guid(core) {
        return Kind::Other("<id>");
    }
    if has_sid(core) {
        return Kind::Other("<sid>");
    }
    if has_address(core) {
        return Kind::Other("<address>");
    }
    if is_long_hex(core) {
        return Kind::Other("<hex>");
    }
    if let Some((stem, ext)) = core.rsplit_once('.')
        && !stem.is_empty()
        && FILE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(ext))
    {
        return Kind::Other("<file>");
    }
    Kind::Plain
}

/// `{...}` holding 8 or more hex digits, dots and dashes
fn has_braced_id(s: &str) -> bool {
    s.split('{').skip(1).any(|part| {
        part.split_once('}').is_some_and(|(inner, _)| {
            inner.len() >= 8 && inner.bytes().all(|c| c.is_ascii_hexdigit() || c == b'.' || c == b'-')
        })
    })
}

/// 8-4-4-4-12 hex digits anywhere in `s`
fn has_guid(s: &str) -> bool {
    let b = s.as_bytes();
    let dash = |i: usize| matches!(i, 8 | 13 | 18 | 23);
    b.windows(36).any(|w| w.iter().enumerate().all(|(i, &c)| if dash(i) { c == b'-' } else { c.is_ascii_hexdigit() }))
}

/// `S-1-` and a digit: a SID, also inside a longer word (a pipe name)
fn has_sid(s: &str) -> bool {
    s.match_indices("S-1-").any(|(i, _)| s.as_bytes().get(i + 4).is_some_and(u8::is_ascii_digit))
}

/// Six pairs of hex digits joined by `:` or `-`
fn has_address(s: &str) -> bool {
    s.as_bytes().windows(17).any(|w| {
        let sep = w[2];
        (sep == b':' || sep == b'-')
            && (0..6).all(|k| w[k * 3].is_ascii_hexdigit() && w[k * 3 + 1].is_ascii_hexdigit())
            && (0..5).all(|k| w[k * 3 + 2] == sep)
    })
}

/// 16 or more hex digits (after an optional `0x`) with at least one letter
fn is_long_hex(s: &str) -> bool {
    let digits = s.strip_prefix("0x").unwrap_or(s);
    digits.len() >= 16
        && digits.bytes().all(|c| c.is_ascii_hexdigit())
        && digits.bytes().any(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines of the agent's code paths as they are written today, and what
    /// the core's log gets
    #[test]
    fn names_ids_and_paths_do_not_reach_the_core() {
        let cases = [
            // session-host host/media/smtc.rs
            (r#"MCP: media player "Spotify Premium": playing"#, "MCP: media player <name>: playing"),
            (
                "MCP media: player \u{201C}SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify\u{201D} is \"Spotify\"",
                "MCP media: player <name> is <name>",
            ),
            (
                r"MCP media: player C:\Program Files\Mozilla Firefox\firefox.exe is <name>",
                "MCP media: player <path> is <name>",
            ),
            // voice-link render.rs and wav.rs
            (
                "voice link: rendering the microphone to \u{201C}Speakers (Realtek(R) Audio)\u{201D}, 48000 Hz, prefill 40 ms",
                "voice link: rendering the microphone to <name>, 48000 Hz, prefill 40 ms",
            ),
            (
                r"voice link: microphone to C:\Users\Jane Mary Doe\AppData\Local\Temp\mic.wav (16000 Hz)",
                "voice link: microphone to <path> (16000 Hz)",
            ),
            (
                r"voice link: C:\Users\jane\mic.wav written, 16000 samples (1.0 s)",
                "voice link: <path> written, 16000 samples (1.0 s)",
            ),
            // audio-capture devices.rs, loopback.rs; win-sys audio_policy.rs
            (
                "audio devices: setting default render endpoint to {0.0.0.00000000}.{a6d5c3f1-1d2e-4b5a-9c8d-0123456789ab}",
                "audio devices: setting default render endpoint to <id>",
            ),
            (
                r#"audio devices: 2 active render endpoints, default Some("{0.0.0.00000000}.{a6d5c3f1-1d2e-4b5a-9c8d-0123456789ab}")"#,
                "audio devices: 2 active render endpoints, default Some(<name>)",
            ),
            (
                r"audio capture: device not found: SWD\MMDEVAPI\{0.0.0.00000000}.{a6d5c3f1-1d2e-4b5a-9c8d-0123456789ab}",
                "audio capture: device not found: <path>",
            ),
            (
                "audio_policy: default input a6d5c3f1-1d2e-4b5a-9c8d-0123456789ab (Console): 0x00000000",
                "audio_policy: default input <id> (Console): 0x00000000",
            ),
            // audio-capture apps.rs, mixer.rs
            ("audio apps: Discord.exe: 2 processes", "audio apps: <file>: 2 processes"),
            (
                "mixer: spotify.exe: volume Some(20), muted None: Ok(1)",
                "mixer: <file>: volume Some(20), muted None: Ok(1)",
            ),
            // win-sys autostart.rs, elevation.rs, pipe.rs
            (
                r#"autostart: write HKCU\Software\Microsoft\Windows\CurrentVersion\Run\BlueCaster = "C:\BlueCaster\bluecaster-core.exe" --background: Ok(())"#,
                "autostart: write <path> = <name> --background: Ok(())",
            ),
            (
                r"pipe: instance of \\.\pipe\bluecaster-agent-S-1-5-21-1004336348-1177238915-682003330-1001 created (first true, owner S-1-5-21-1004336348-1177238915-682003330-1001)",
                "pipe: instance of <path> created (first true, owner <sid>)",
            ),
            // The log header: bluecaster-core 3.0.3 (x86_64) starting: ..., <exe path>
            (
                r"bluecaster-core 3.0.3 (x86_64) starting: pid 4, Windows build 26200, C:\Users\jane\AppData\Local\Programs\BlueCaster\bluecaster-core.exe",
                "bluecaster-core 3.0.3 (x86_64) starting: pid 4, Windows build 26200, <path>",
            ),
            ("file:///C:/Users/jane/x.wav opened", "<path> opened"),
            ("C:/Users/jane/x.wav opened", "<path> opened"),
            // Should never be there, and are not forwarded if they are
            ("key for 00:1A:7D:DA:71:13 stored", "key for <address> stored"),
            ("link key 0123456789abcdef0123456789abcdef", "link key <hex>"),
        ];
        for (line, want) in cases {
            assert_eq!(reduce(line), want, "{line}");
            // The core reduces again what the agent reduced: no change
            assert_eq!(reduce(want), want, "{want}");
        }
    }

    #[test]
    fn counts_and_states_are_kept() {
        for line in [
            "capture agent: channel opened (2 open)",
            "MCP: dry run: PLAY (nothing sent to Windows)",
            "media key PlayPause: dry run, not sent",
            "shell (test): tray model 1 with 1 menu item(s)",
            "render: channel closed, 12 record(s) read (0 open)",
            "audio capture started: 48000 Hz, 2 ch, F32, event mode, period 10 ms",
            "capture agent: open failed (DeviceNotFound, HRESULT Some(80070490))",
            "audio capture: loop running, wait 10 ms, qpc 1234567890123456789, buffer 3840 bytes",
            "voice link: render last 10 s: 480000 frames, 0 silence, 0 underruns, 0 overflow",
            "session agent: lost (process 4242, the pipe was closed), 0 request(s) failed",
            "12:34:56 tick, version 3.0.3, ratio 1.5.",
        ] {
            assert_eq!(reduce(line), line);
        }
    }

    #[test]
    fn no_forged_lines_and_no_endless_lines() {
        assert_eq!(
            reduce("done\n2026-11-22 12:00:00.000  INFO bluecaster_core: fake"),
            "done 2026-11-22 12:00:00.000  INFO bluecaster_core: fake"
        );
        assert_eq!(reduce("a\u{202E}b\u{200B}c\r\u{1b}[31m"), "a b c  [31m");
        let long = "x".repeat(MAX_CHARS * 3);
        let cut = reduce(&long);
        assert_eq!(cut.chars().count(), MAX_CHARS + 3);
        assert!(cut.ends_with("..."));
        // Multi-byte characters are cut whole
        assert_eq!(reduce(&"\u{3042}".repeat(MAX_CHARS + 1)).chars().count(), MAX_CHARS + 3);
    }

    #[test]
    fn quotes_without_an_end_take_the_rest() {
        assert_eq!(reduce(r#"player "Spot\"ify" ok"#), "player <name> ok");
        assert_eq!(reduce(r#"player "Spotify: playing"#), "player <name>");
        assert_eq!(reduce("player \u{201C}Spotify: playing"), "player <name>");
        assert_eq!(reduce(""), "");
        assert_eq!(reduce("  two  spaces "), "  two  spaces ");
    }
}
