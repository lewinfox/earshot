//! Track identity: a SHA-256 of a file's audio, leaving out its tags, so retagging a
//! file keeps its hash and a moved file can be matched to the track it was.
//!
//! MP3 (and raw AAC) use the convention shared with ~/git/cratelib and
//! ~/git/music-sync: the file's bytes without the ID3v2 block at the start (and its
//! footer, if flagged) or an ID3v1 "TAG" block at the end. Those tools also write the
//! hash into the file as an ID3 `TXXX:AUDIO_SHA256` frame (hex), which [`tag_hash`]
//! reads, so the scan doesn't have to read whole files.
//!
//! Other formats have no shared convention, so these are ours (nothing is ever
//! written back to the files):
//! - MP4 / M4A (AAC, ALAC): the payload of every top-level `mdat` box, in order.
//! - FLAC: everything after the metadata blocks (and before an ID3v1 block).
//! - WAV: the `data` chunk's payload. AIFF: the `SSND` chunk's payload.
//! - Anything else (Ogg, or a file we can't parse): the whole file.

use std::fs::File;
use std::io::{self, SeekFrom};
use std::path::Path;

use earshot_engine::media::{Location, Media};

use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];

/// The ID3 `TXXX` description cratelib and music-sync store the hash under.
pub const AUDIO_HASH: &str = "AUDIO_SHA256";

/// The audio hash of any supported track, by its extension.
pub fn content_hash(at: &Location) -> io::Result<Hash> {
    let ext = at.extension().unwrap_or_default();
    let mut f = at.open().map_err(io::Error::other)?;
    let f = f.as_mut();
    let size = match f.len() {
        Some(n) => n,
        None => f.seek(SeekFrom::End(0))?,
    };
    let ranges = match ext.as_str() {
        "mp3" | "aac" => vec![id3_audio_range(f, size)?],
        "m4a" | "mp4" | "m4b" | "alac" => mp4_mdat(f, size)?,
        "flac" => flac_frames(f, size)?,
        "wav" => riff_chunk(f, size, false)?,
        "aif" | "aiff" | "aifc" => riff_chunk(f, size, true)?,
        _ => vec![],
    };
    let ranges = if ranges.is_empty() {
        vec![(0, size)]
    } else {
        ranges
    };
    hash_ranges(f, &ranges)
}

/// cratelib's `audio_sha256`, byte for byte: SHA-256 of an MP3 without its ID3v2 block
/// (and footer) at the start and any 128-byte ID3v1 block at the end.
#[allow(dead_code)] // library management API, not wired to the UI yet (tests use it)
pub fn audio_sha256(path: &Path) -> io::Result<Hash> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let range = id3_audio_range(&mut f, size)?;
    hash_ranges(&mut f, &[range])
}

/// Lower-case hex, as cratelib writes it.
#[allow(dead_code)] // library management API, not wired to the UI yet (tests use it)
pub fn to_hex(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<Hash> {
    let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// (start, end) of the bytes [`audio_sha256`] hashes. Mirrors cratelib exactly,
/// including when the ID3 size runs past the end of the file (nothing is hashed).
fn id3_audio_range(f: &mut dyn Media, size: u64) -> io::Result<(u64, u64)> {
    let mut head = [0u8; 10];
    let n = read_up_to(f, &mut head)?;
    let mut start = 0u64;
    if n == 10 && &head[..3] == b"ID3" {
        start = 10 + syncsafe(&head[6..10]) + if head[5] & 0x10 != 0 { 10 } else { 0 };
    }
    let mut end = size;
    if size >= start + 128 {
        f.seek(SeekFrom::Start(size - 128))?;
        let mut tag = [0u8; 3];
        if read_up_to(f, &mut tag)? == 3 && &tag == b"TAG" {
            end -= 128;
        }
    }
    Ok((start, end.max(start)))
}

/// Payloads of the top-level `mdat` boxes.
fn mp4_mdat(f: &mut dyn Media, size: u64) -> io::Result<Vec<(u64, u64)>> {
    let mut out = vec![];
    let mut at = 0u64;
    while at + 8 <= size {
        f.seek(SeekFrom::Start(at))?;
        let mut h = [0u8; 16];
        if read_up_to(f, &mut h[..8])? < 8 {
            break;
        }
        let mut len = u32::from_be_bytes(h[..4].try_into().unwrap()) as u64;
        let mut header = 8;
        if len == 1 {
            if read_up_to(f, &mut h[8..16])? < 8 {
                break;
            }
            len = u64::from_be_bytes(h[8..16].try_into().unwrap());
            header = 16;
        } else if len == 0 {
            len = size - at; // to the end of the file
        }
        if len < header {
            return Ok(vec![]); // not an MP4 we understand: hash the whole file
        }
        let end = (at + len).min(size);
        if &h[4..8] == b"mdat" {
            out.push((at + header, end));
        }
        at += len;
    }
    Ok(out)
}

/// The audio frames after a FLAC's metadata blocks.
fn flac_frames(f: &mut dyn Media, size: u64) -> io::Result<Vec<(u64, u64)>> {
    // Some taggers put an ID3v2 block in front.
    let (mut at, end) = id3_audio_range(f, size)?;
    f.seek(SeekFrom::Start(at))?;
    let mut magic = [0u8; 4];
    if read_up_to(f, &mut magic)? < 4 || &magic != b"fLaC" {
        return Ok(vec![]);
    }
    at += 4;
    loop {
        let mut h = [0u8; 4];
        if read_up_to(f, &mut h)? < 4 {
            return Ok(vec![]);
        }
        let len = u32::from_be_bytes([0, h[1], h[2], h[3]]) as u64;
        at += 4 + len;
        if h[0] & 0x80 != 0 {
            break;
        }
        f.seek(SeekFrom::Start(at))?;
    }
    Ok(if at <= end { vec![(at, end)] } else { vec![] })
}

/// WAV's `data` chunk (little-endian sizes) or AIFF's `SSND` (big-endian).
fn riff_chunk(f: &mut dyn Media, size: u64, aiff: bool) -> io::Result<Vec<(u64, u64)>> {
    let mut h = [0u8; 12];
    if read_up_to(f, &mut h)? < 12 {
        return Ok(vec![]);
    }
    let (magic, want): (&[u8], &[u8]) = if aiff {
        (b"FORM", b"SSND")
    } else {
        (b"RIFF", b"data")
    };
    if &h[..4] != magic {
        return Ok(vec![]);
    }
    let mut at = 12u64;
    while at + 8 <= size {
        f.seek(SeekFrom::Start(at))?;
        let mut c = [0u8; 8];
        if read_up_to(f, &mut c)? < 8 {
            break;
        }
        let b: [u8; 4] = c[4..8].try_into().unwrap();
        let len = if aiff {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        } as u64;
        if &c[..4] == want {
            return Ok(vec![(at + 8, (at + 8 + len).min(size))]);
        }
        at += 8 + len + (len & 1); // chunks are padded to an even length
    }
    Ok(vec![])
}

fn hash_ranges(f: &mut dyn Media, ranges: &[(u64, u64)]) -> io::Result<Hash> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    for &(start, end) in ranges {
        f.seek(SeekFrom::Start(start))?;
        let mut left = end.saturating_sub(start);
        while left > 0 {
            let want = left.min(buf.len() as u64) as usize;
            let n = f.read(&mut buf[..want])?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            left -= n as u64;
        }
    }
    Ok(h.finalize().into())
}

fn read_up_to(f: &mut dyn Media, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Four bytes of seven bits each, as ID3v2 sizes are.
fn syncsafe(b: &[u8]) -> u64 {
    b.iter().fold(0, |acc, &x| (acc << 7) | (x & 0x7f) as u64)
}

// ---- reading TXXX:AUDIO_SHA256 ----------------------------------------------------------

/// The hash stored in an MP3's (or AAC's) `TXXX:AUDIO_SHA256` frame, if it has one.
/// Reads only frame headers, skipping over pictures, so it's cheap enough for a scan.
pub fn tag_hash(at: &Location) -> Option<Hash> {
    let ext = at.extension()?;
    if ext != "mp3" && ext != "aac" {
        return None;
    }
    let mut f = at.open().ok()?;
    read_txxx(f.as_mut(), AUDIO_HASH).and_then(|v| from_hex(&v))
}

/// The value of the first ID3v2 `TXXX` frame with this description.
fn read_txxx(f: &mut dyn Media, desc: &str) -> Option<String> {
    let mut head = [0u8; 10];
    if read_up_to(f, &mut head).ok()? < 10 || &head[..3] != b"ID3" {
        return None;
    }
    let version = head[3];
    let end = 10 + syncsafe(&head[6..10]);
    let mut at = 10u64;
    // The extended header: v2.3's size leaves out its own 4 bytes, v2.4's doesn't.
    if head[5] & 0x40 != 0 {
        let mut b = [0u8; 4];
        read_up_to(f, &mut b).ok()?;
        at += match version {
            3 => 4 + u32::from_be_bytes(b) as u64,
            4 => syncsafe(&b),
            _ => return None,
        };
    }
    let (id_len, header_len, want): (usize, u64, &[u8]) = match version {
        2 => (3, 6, b"TXX"),
        3 | 4 => (4, 10, b"TXXX"),
        _ => return None,
    };
    while at + header_len <= end {
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut h = [0u8; 10];
        read_up_to(f, &mut h[..header_len as usize]).ok()?;
        if h[0] == 0 {
            return None; // padding
        }
        let s = &h[id_len..];
        let len = match version {
            2 => u32::from_be_bytes([0, s[0], s[1], s[2]]) as u64,
            3 => u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as u64,
            _ => syncsafe(&s[..4]),
        };
        at += header_len;
        if &h[..id_len] == want && len < 4096 {
            let mut body = vec![0u8; len as usize];
            read_up_to(f, &mut body).ok()?;
            // v2.4's "data length indicator" flag puts 4 extra bytes first.
            if version == 4 && h[9] & 0x01 != 0 && body.len() >= 4 {
                body.drain(..4);
            }
            if let Some((d, v)) = txxx_fields(&body)
                && d == desc
            {
                return Some(v);
            }
        }
        at += len;
    }
    None
}

/// (description, value) of a TXXX frame's body.
fn txxx_fields(body: &[u8]) -> Option<(String, String)> {
    let (&enc, rest) = body.split_first()?;
    match enc {
        // Latin-1 and UTF-8 end the description with one zero byte.
        0 | 3 => {
            let i = rest.iter().position(|&b| b == 0)?;
            let text = |b: &[u8]| match enc {
                0 => b.iter().map(|&c| c as char).collect::<String>(),
                _ => String::from_utf8_lossy(b).into_owned(),
            };
            Some((text(&rest[..i]), text(&rest[i + 1..])))
        }
        // UTF-16 (with a byte-order mark) and UTF-16BE end it with two.
        1 | 2 => {
            let i = rest
                .chunks_exact(2)
                .position(|c| c == [0, 0])
                .map(|i| i * 2)?;
            Some((
                utf16(&rest[..i], enc == 2),
                utf16(&rest[(i + 2).min(rest.len())..], enc == 2),
            ))
        }
        _ => None,
    }
}

fn utf16(b: &[u8], mut big_endian: bool) -> String {
    let mut b = b;
    if b.len() >= 2 && !big_endian {
        match [b[0], b[1]] {
            [0xff, 0xfe] => b = &b[2..],
            [0xfe, 0xff] => {
                big_endian = true;
                b = &b[2..];
            }
            _ => {}
        }
    }
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| {
            if big_endian {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name)
    }

    fn temp(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("earshot-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn content_hash_at(p: &Path) -> io::Result<Hash> {
        content_hash(&p.into())
    }

    fn tag_hash_at(p: &Path) -> Option<Hash> {
        tag_hash(&p.into())
    }

    fn sha(b: &[u8]) -> Hash {
        Sha256::digest(b).into()
    }

    /// Expected hashes from cratelib itself (`gui/testdata/make_fixtures.py` made them
    /// and the files): v2.3 and v2.4 tags, a picture frame before the TXXX, and an ID3v1
    /// block at the end.
    #[test]
    fn matches_cratelib() {
        let expected = std::fs::read_to_string(fixture("expected.txt")).unwrap();
        let mut n = 0;
        for line in expected.lines().filter(|l| !l.is_empty()) {
            let (name, hex) = line.split_once(' ').unwrap();
            let path = fixture(name);
            assert_eq!(to_hex(&audio_sha256(&path).unwrap()), hex, "{name}");
            assert_eq!(to_hex(&content_hash_at(&path).unwrap()), hex, "{name}");
            n += 1;
        }
        assert!(n >= 3);
    }

    #[test]
    fn reads_the_stamped_hash() {
        let expected = std::fs::read_to_string(fixture("expected.txt")).unwrap();
        for line in expected.lines().filter(|l| !l.is_empty()) {
            let (name, hex) = line.split_once(' ').unwrap();
            let stamped = tag_hash_at(&fixture(name));
            if name.contains("stamped") {
                assert_eq!(stamped.map(|h| to_hex(&h)).as_deref(), Some(hex), "{name}");
            } else {
                assert_eq!(stamped, None, "{name}");
            }
        }
    }

    /// The same rules worked out independently, on hand-built files: a footer flag, a
    /// tag-less file, and an ID3 size that runs past the end.
    #[test]
    fn id3_edges() {
        let audio = b"\xff\xfbaudio frames here".repeat(20);
        let mut tagged = b"ID3\x04\x00\x10\x00\x00\x00\x05".to_vec(); // footer flag, 5-byte body
        tagged.extend(b"abcde");
        tagged.extend(b"3DI\x04\x00\x10\x00\x00\x00\x05"); // the footer
        tagged.extend(&audio);
        let mut v1 = b"TAG".to_vec();
        v1.resize(128, b' ');
        tagged.extend(&v1);
        assert_eq!(
            audio_sha256(&temp("footer.mp3", &tagged)).unwrap(),
            sha(&audio)
        );
        assert_eq!(
            audio_sha256(&temp("bare.mp3", &audio)).unwrap(),
            sha(&audio)
        );
        let short = b"ID3\x03\x00\x00\x00\x00\x7f\x7fxyz";
        assert_eq!(audio_sha256(&temp("short.mp3", short)).unwrap(), sha(b""));
    }

    #[test]
    fn mp4_hashes_only_mdat() {
        let mdat = b"some aac frames".to_vec();
        let file = |ilst: &[u8]| {
            let mut f = vec![];
            for (kind, body) in [(b"ftyp", &b"M4A "[..]), (b"moov", ilst), (b"mdat", &mdat)] {
                f.extend((8 + body.len() as u32).to_be_bytes());
                f.extend(kind);
                f.extend(body);
            }
            f
        };
        let a = content_hash_at(&temp("a.m4a", &file(b"title one"))).unwrap();
        let b = content_hash_at(&temp("b.m4a", &file(b"a much longer title two"))).unwrap();
        assert_eq!(a, sha(&mdat));
        assert_eq!(a, b, "retagging doesn't change the hash");
    }

    #[test]
    fn flac_and_wav_skip_their_metadata() {
        let frames = b"\xff\xf8flac frames".to_vec();
        let flac = |comment: &[u8]| {
            let mut f = b"fLaC".to_vec();
            f.extend([0x00, 0, 0, 2]); // STREAMINFO (shortened), not last
            f.extend(b"si");
            f.push(0x84); // VORBIS_COMMENT, last
            f.extend(&(comment.len() as u32).to_be_bytes()[1..]);
            f.extend(comment);
            f.extend(&frames);
            f
        };
        assert_eq!(
            content_hash_at(&temp("a.flac", &flac(b"x"))).unwrap(),
            sha(&frames)
        );
        assert_eq!(
            content_hash_at(&temp("b.flac", &flac(b"longer"))).unwrap(),
            sha(&frames)
        );

        let pcm = b"0123456789".to_vec();
        let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
        wav.extend(b"LIST\x03\0\0\0abc\0"); // odd length: padded
        wav.extend(b"data\x0a\0\0\0");
        wav.extend(&pcm);
        assert_eq!(content_hash_at(&temp("a.wav", &wav)).unwrap(), sha(&pcm));
    }

    #[test]
    fn hex_round_trip() {
        let h = sha(b"x");
        assert_eq!(from_hex(&to_hex(&h)), Some(h));
        assert_eq!(from_hex(&format!("{}\0", to_hex(&h))), Some(h));
        assert_eq!(from_hex("abc"), None);
    }
}
