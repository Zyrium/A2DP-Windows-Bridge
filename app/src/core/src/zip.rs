//! A small .zip writer (deflate, no ZIP64) for the diagnostic bundle: the
//! files are read into memory first, and each stays well under 4 GB. And a
//! reader of the same kind of archive ([`read_all`]), for VB-Audio's driver
//! pack (vb_cable.rs).

use std::io::{Read, Write};

use flate2::Compression;
use flate2::write::DeflateEncoder;

/// UTF-8 names (general purpose flag bit 11)
const UTF8: u16 = 0x0800;
const VERSION: u16 = 20;

struct Entry {
    name: Vec<u8>,
    /// 0 stored, 8 deflate
    method: u16,
    crc: u32,
    packed: u32,
    size: u32,
    offset: u32,
}

/// The archive, built in memory
pub struct ZipWriter {
    out: Vec<u8>,
    entries: Vec<Entry>,
    /// MS-DOS time and date of every entry
    dos: (u16, u16),
}

impl ZipWriter {
    /// `local`: the entries' time (year, month, day, hour, minute, second)
    pub fn new(local: (u16, u8, u8, u8, u8, u8)) -> ZipWriter {
        let (y, mo, d, h, mi, s) = local;
        let time = (u16::from(h) << 11) | (u16::from(mi) << 5) | u16::from(s / 2);
        let date = (y.saturating_sub(1980) << 9) | (u16::from(mo) << 5) | u16::from(d);
        ZipWriter { out: Vec::new(), entries: Vec::new(), dos: (time, date) }
    }

    /// Adds a file; `name` uses forward slashes
    pub fn add(&mut self, name: &str, data: &[u8]) {
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        let packed = enc.write_all(data).and_then(|()| enc.finish()).unwrap_or_default();
        // Stored when deflate does not help (dumps, captures)
        let (method, body) =
            if !packed.is_empty() && packed.len() < data.len() { (8, packed.as_slice()) } else { (0, data) };
        let e = Entry {
            name: name.as_bytes().to_vec(),
            method,
            crc: crc32fast::hash(data),
            packed: body.len() as u32,
            size: data.len() as u32,
            offset: self.out.len() as u32,
        };
        let o = &mut self.out;
        put32(o, 0x0403_4b50);
        put16(o, VERSION);
        put16(o, UTF8);
        put16(o, e.method);
        put16(o, self.dos.0);
        put16(o, self.dos.1);
        put32(o, e.crc);
        put32(o, e.packed);
        put32(o, e.size);
        put16(o, e.name.len() as u16);
        put16(o, 0); // extra field
        o.extend_from_slice(&e.name);
        o.extend_from_slice(body);
        self.entries.push(e);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The entries' names, in the order added
    #[cfg(test)]
    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| String::from_utf8_lossy(&e.name).into_owned()).collect()
    }

    /// The archive's bytes
    pub fn finish(mut self) -> Vec<u8> {
        let start = self.out.len() as u32;
        let o = &mut self.out;
        for e in &self.entries {
            put32(o, 0x0201_4b50);
            put16(o, VERSION); // made by
            put16(o, VERSION); // needed
            put16(o, UTF8);
            put16(o, e.method);
            put16(o, self.dos.0);
            put16(o, self.dos.1);
            put32(o, e.crc);
            put32(o, e.packed);
            put32(o, e.size);
            put16(o, e.name.len() as u16);
            put16(o, 0); // extra field
            put16(o, 0); // comment
            put16(o, 0); // disk
            put16(o, 0); // internal attributes
            put32(o, 0); // external attributes
            put32(o, e.offset);
            o.extend_from_slice(&e.name);
        }
        let size = o.len() as u32 - start;
        let n = self.entries.len() as u16;
        put32(o, 0x0605_4b50);
        put16(o, 0); // this disk
        put16(o, 0); // the directory's disk
        put16(o, n);
        put16(o, n);
        put32(o, size);
        put32(o, start);
        put16(o, 0); // comment
        self.out
    }
}

fn put16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn put32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}

/// The files of an archive held in memory: (name, contents), stored or
/// deflated entries only, each checked against its CRC-32. Directory
/// entries are left out. None for anything else (ZIP64, encryption, a
/// broken archive).
pub fn read_all(zip: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let u16_at = |at: usize| zip.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| zip.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    // End of central directory: the last signature, within its comment's reach
    let eocd = (0..=zip.len().checked_sub(22)?).rev().take(22 + 0xFFFF).find(|&i| u32_at(i) == Some(0x0605_4b50))?;
    let count = usize::from(u16_at(eocd + 10)?);
    let mut at = u32_at(eocd + 16)? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(at)? != 0x0201_4b50 {
            return None;
        }
        let flags = u16_at(at + 8)?;
        let method = u16_at(at + 10)?;
        let crc = u32_at(at + 16)?;
        let packed = u32_at(at + 20)? as usize;
        let size = u32_at(at + 24)? as usize;
        let name_len = usize::from(u16_at(at + 28)?);
        let extra_len = usize::from(u16_at(at + 30)?);
        let comment_len = usize::from(u16_at(at + 32)?);
        let local = u32_at(at + 42)? as usize;
        let name = String::from_utf8_lossy(zip.get(at + 46..at + 46 + name_len)?).into_owned();
        at += 46 + name_len + extra_len + comment_len;
        if flags & 1 != 0 {
            return None;
        }
        if name.ends_with('/') {
            continue;
        }
        if u32_at(local)? != 0x0403_4b50 {
            return None;
        }
        let start = local + 30 + usize::from(u16_at(local + 26)?) + usize::from(u16_at(local + 28)?);
        let body = zip.get(start..start.checked_add(packed)?)?;
        let data = match method {
            0 => body.to_vec(),
            8 => {
                let mut data = Vec::with_capacity(size);
                flate2::read::DeflateDecoder::new(body).take(size as u64 + 1).read_to_end(&mut data).ok()?;
                data
            }
            _ => return None,
        };
        if data.len() != size || crc32fast::hash(&data) != crc {
            return None;
        }
        out.push((name, data));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_what_it_writes() {
        let mut z = ZipWriter::new((2026, 11, 11, 12, 0, 0));
        let text = b"VB-CABLE ".repeat(100);
        z.add("readme.txt", &text);
        z.add("setup.exe", &[0x4d, 0x5a, 0x90, 0x00]);
        let files = read_all(&z.finish()).unwrap();
        assert_eq!(files, vec![("readme.txt".into(), text), ("setup.exe".into(), vec![0x4d, 0x5a, 0x90, 0x00])]);
    }

    #[test]
    fn refuses_a_broken_archive() {
        let mut z = ZipWriter::new((2026, 11, 11, 12, 0, 0));
        z.add("a.txt", &b"hello hello hello hello".repeat(10));
        let mut bytes = z.finish();
        assert!(read_all(&bytes[..bytes.len() - 1]).is_none());
        bytes[40] ^= 0xFF;
        assert!(read_all(&bytes).is_none());
        assert!(read_all(b"").is_none());
    }

    /// The archive opens with Windows' own reader (tar.exe, bsdtar) and
    /// gives the files back
    #[test]
    fn round_trips_through_tar() {
        let mut z = ZipWriter::new((2026, 10, 21, 12, 34, 56));
        let text = "hello ".repeat(1000);
        z.add("a.txt", text.as_bytes());
        z.add("dir/b.bin", &[1, 2, 3]);
        assert_eq!(z.len(), 2);
        let dir = std::env::temp_dir().join(format!("bluecaster-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.zip");
        std::fs::write(&path, z.finish()).unwrap();
        // CREATE_NO_WINDOW: no terminal window pops up on the desktop
        let status = std::os::windows::process::CommandExt::creation_flags(
            std::process::Command::new("tar").arg("-xf").arg(&path).arg("-C").arg(&dir),
            0x0800_0000,
        )
        .status()
        .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), text);
        assert_eq!(std::fs::read(dir.join("dir").join("b.bin")).unwrap(), [1, 2, 3]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
