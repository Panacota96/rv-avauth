//! MPEG audio (MP3) with ID3: ID3v2.2/2.3/2.4 tag (text frames incl. TSSE, TENC, dates), ID3v1 and APEv2
//! at the end, the Xing / Info / VBRI header and LAME tag of the first frame, and a frame-sync walk over
//! every frame (count, bitrates → CBR/VBR, padding, parameter changes, sync gaps, truncated last frame).
//! Audio is never decoded.

use crate::model::{AnomalyKind, MediaInfo, Mp3Info, Stream, TimeValue, XingHeader, limits};
use crate::source::{Source, read_array};
use crate::util::{be32, latin1, rfc3339};

/// A parsed 4-byte MPEG audio frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    /// 1 (MPEG-1), 2 (MPEG-2), 25 (MPEG-2.5)
    pub version: u8,
    pub layer: u8,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub padding: bool,
    /// 0 stereo, 1 joint stereo, 2 dual channel, 3 mono
    pub channel_mode: u8,
    /// Frame length in bytes (header included).
    pub length: u32,
    pub samples: u32,
}

const BR_V1_L1: [u32; 15] = [
    0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
];
const BR_V1_L2: [u32; 15] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
];
const BR_V1_L3: [u32; 15] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const BR_V2_L1: [u32; 15] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
];
const BR_V2_L23: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

impl FrameHeader {
    /// Parses the header at the start of `b`; None when it is not a valid, non-free-format header.
    pub fn parse(b: &[u8]) -> Option<FrameHeader> {
        if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
            return None;
        }
        let version = match (b[1] >> 3) & 3 {
            0 => 25,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let layer = match (b[1] >> 1) & 3 {
            1 => 3,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let bri = (b[2] >> 4) as usize;
        let sri = ((b[2] >> 2) & 3) as usize;
        if bri == 0 || bri == 15 || sri == 3 {
            return None;
        }
        let bitrate = match (version, layer) {
            (1, 1) => BR_V1_L1[bri],
            (1, 2) => BR_V1_L2[bri],
            (1, _) => BR_V1_L3[bri],
            (_, 1) => BR_V2_L1[bri],
            _ => BR_V2_L23[bri],
        };
        let base = [44_100u32, 48_000, 32_000][sri];
        let sample_rate = match version {
            1 => base,
            2 => base / 2,
            _ => base / 4,
        };
        let padding = (b[2] >> 1) & 1 == 1;
        let samples = match (version, layer) {
            (_, 1) => 384,
            (1, _) | (_, 2) => 1152,
            _ => 576,
        };
        let pad = padding as u32;
        let length = if layer == 1 {
            (12 * bitrate * 1000 / sample_rate + pad) * 4
        } else {
            samples / 8 * bitrate * 1000 / sample_rate + pad
        };
        if length < 4 {
            return None;
        }
        Some(FrameHeader {
            version,
            layer,
            bitrate_kbps: bitrate,
            sample_rate,
            padding,
            channel_mode: b[3] >> 6,
            length,
            samples,
        })
    }

    pub fn channels(&self) -> u16 {
        if self.channel_mode == 3 { 1 } else { 2 }
    }
}

fn mode_name(m: u8) -> &'static str {
    match m {
        0 => "stereo",
        1 => "joint stereo",
        2 => "dual channel",
        _ => "mono",
    }
}

fn syncsafe(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |a, &c| (a << 7) | (c & 0x7F) as u64)
}

/// Decodes an ID3v2 text frame body (encoding byte + text).
fn id3_text(body: &[u8]) -> String {
    let Some((&enc, t)) = body.split_first() else {
        return String::new();
    };
    let s = match enc {
        1 | 2 => {
            let (le, t) = if t.starts_with(&[0xFF, 0xFE]) {
                (true, &t[2..])
            } else if t.starts_with(&[0xFE, 0xFF]) {
                (false, &t[2..])
            } else {
                (enc == 1, t)
            };
            let u: Vec<u16> = t
                .chunks_exact(2)
                .map(|c| {
                    if le {
                        u16::from_le_bytes([c[0], c[1]])
                    } else {
                        u16::from_be_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&u)
        }
        3 => String::from_utf8_lossy(t).into_owned(),
        _ => latin1(t),
    };
    // Multiple values are NUL-separated: keep them readable.
    s.trim_end_matches('\0').replace('\0', " / ")
}

/// Parses a complete ID3v2 tag held in memory (MP3 start or a WAVE `id3 ` chunk). Returns the tag size.
pub fn id3v2_from_bytes(d: &[u8], info: &mut MediaInfo) -> Option<u64> {
    if d.len() < 10 || &d[0..3] != b"ID3" {
        return None;
    }
    let major = d[3];
    let flags = d[5];
    let size = syncsafe(&d[6..10]);
    let total = 10 + size + if flags & 0x10 != 0 { 10 } else { 0 };
    let source = format!("ID3v2.{}", major);
    if flags & 0x80 != 0 {
        info.note(format!(
            "{} uses unsynchronisation: frames are read as stored",
            source
        ));
    }
    let end = (10 + size as usize).min(d.len());
    let mut at = 10usize;
    if flags & 0x40 != 0 && major >= 3 {
        // Extended header: its size is in the first 4 bytes (syncsafe in 2.4).
        let ext = if major == 4 {
            syncsafe(d.get(10..14).unwrap_or(&[0; 4])) as usize
        } else {
            be32(d, 10).unwrap_or(0) as usize + 4
        };
        at += ext;
    }
    let mut frames = 0;
    while at + 10 <= end && frames < 1024 {
        frames += 1;
        let (id, fsize, hdr) = if major == 2 {
            if at + 6 > end {
                break;
            }
            let id = latin1(&d[at..at + 3]);
            let s = ((d[at + 3] as usize) << 16) | ((d[at + 4] as usize) << 8) | d[at + 5] as usize;
            (id, s, 6)
        } else {
            let id = latin1(&d[at..at + 4]);
            let s = if major == 4 {
                syncsafe(&d[at + 4..at + 8]) as usize
            } else {
                be32(d, at + 4).unwrap_or(0) as usize
            };
            (id, s, 10)
        };
        if id.bytes().all(|c| c == 0) {
            break; // padding
        }
        if !id
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            info.anomaly(
                AnomalyKind::SizeInconsistent,
                at as u64,
                format!("{}: invalid frame id after {} frames", source, frames - 1),
            );
            break;
        }
        if at + hdr + fsize > end {
            info.anomaly(
                AnomalyKind::SizeInconsistent,
                at as u64,
                format!(
                    "{} frame {} ({} bytes) runs past the tag",
                    source, id, fsize
                ),
            );
            break;
        }
        let body = &d[at + hdr..at + hdr + fsize];
        let key = id.clone();
        if id.starts_with('T') && id != "TXXX" && id != "TXX" {
            let v = id3_text(body);
            info.tag(&source, &key, &v);
            match id.as_str() {
                "TSSE" | "TSS" | "TENC" | "TEN" => info.encoder(&format!("{} {}", source, id), &v),
                "TDRC" | "TDEN" | "TDTG" | "TYER" | "TYE" | "TDOR" => {
                    let p = crate::util::parse_tag_date(&v);
                    info.time(TimeValue {
                        source: format!("{} {}", source, id),
                        raw: crate::util::clean_text(&v),
                        unix: p.map(|x| x.0),
                        utc: p.and_then(|x| rfc3339(x.0)),
                        origin: if p.is_some_and(|x| x.1) {
                            "tag"
                        } else {
                            "tag_local"
                        }
                        .into(),
                    });
                }
                "TLEN" | "TLE" => {
                    if let Ok(ms) = v.trim().parse::<u64>() {
                        info.duration(&format!("{} TLEN", source), ms, "declared");
                    }
                }
                _ => {}
            }
        } else if id == "TXXX" || id == "TXX" {
            let v = id3_text(body);
            let (k, val) = v.split_once(" / ").unwrap_or(("", v.as_str()));
            info.tag(&source, &format!("TXXX:{}", k), val);
            let kl = k.to_ascii_lowercase();
            if kl.contains("encod") || kl.contains("software") {
                info.encoder(&format!("{} TXXX:{}", source, k), val);
            }
        } else if id == "COMM" || id == "COM" {
            if body.len() > 4 {
                let mut b = vec![body[0]];
                b.extend_from_slice(&body[4..]);
                info.tag(&source, &key, &id3_text(&b));
            }
        } else if id == "APIC" || id == "PIC" {
            info.tag(&source, &key, &format!("[picture, {} bytes]", fsize));
        } else if id == "PRIV" {
            let owner: Vec<u8> = body
                .iter()
                .take_while(|&&c| c != 0)
                .take(128)
                .copied()
                .collect();
            info.tag(&source, &key, &format!("owner {}", latin1(&owner)));
        } else {
            info.tag(&source, &key, &format!("[{} bytes]", fsize));
        }
        at += hdr + fsize;
    }
    Some(total)
}

/// Xing / Info / VBRI header and the LAME tag inside the first frame.
fn xing(frame: &[u8], h: &FrameHeader) -> Option<XingHeader> {
    let side = match (h.version, h.channel_mode == 3) {
        (1, false) => 32,
        (1, true) => 17,
        (_, false) => 17,
        (_, true) => 9,
    };
    let at = 4 + side;
    if let Some(tag) = frame.get(at..at + 4)
        && (tag == b"Xing" || tag == b"Info")
    {
        let flags = be32(frame, at + 4).unwrap_or(0);
        let mut p = at + 8;
        let mut x = XingHeader {
            tag: latin1(tag),
            ..XingHeader::default()
        };
        if flags & 1 != 0 {
            x.frames = be32(frame, p);
            p += 4;
        }
        if flags & 2 != 0 {
            x.bytes = be32(frame, p);
            p += 4;
        }
        if flags & 4 != 0 {
            p += 100;
        }
        if flags & 8 != 0 {
            p += 4;
        }
        if let Some(e) = frame.get(p..p + 9) {
            let s: String = e
                .iter()
                .take_while(|c| (0x20..0x7f).contains(*c))
                .map(|&c| c as char)
                .collect();
            if s.len() >= 4 {
                x.encoder = Some(s.trim().to_string());
            }
        }
        return Some(x);
    }
    if frame.get(36..40) == Some(b"VBRI") {
        return Some(XingHeader {
            tag: "VBRI".into(),
            bytes: be32(frame, 36 + 10),
            frames: be32(frame, 36 + 14),
            encoder: None,
        });
    }
    None
}

/// Next valid frame at or after `from` (two consecutive headers required), scanning at most
/// `MAX_RESYNC_SCAN` bytes and never past `end`.
fn resync(src: &mut dyn Source, from: u64, end: u64) -> Option<(u64, FrameHeader)> {
    let stop = from.saturating_add(limits::MAX_RESYNC_SCAN).min(end);
    let mut at = from;
    while at < stop {
        let chunk = src.read_upto(at, 64 * 1024, 64 * 1024);
        if chunk.len() < 4 {
            return None;
        }
        for i in 0..chunk.len().saturating_sub(3) {
            if chunk[i] != 0xFF {
                continue;
            }
            let pos = at + i as u64;
            if pos >= stop {
                return None;
            }
            if let Some(h) = FrameHeader::parse(&chunk[i..]) {
                let next = pos + h.length as u64;
                if next + 4 > end {
                    // A last frame: accept it when it ends exactly at the end.
                    if next == end {
                        return Some((pos, h));
                    }
                    continue;
                }
                if let Some(nb) = read_array::<4>(src, next)
                    && let Some(h2) = FrameHeader::parse(&nb)
                    && h2.version == h.version
                    && h2.layer == h.layer
                    && h2.sample_rate == h.sample_rate
                {
                    return Some((pos, h));
                }
            }
        }
        at += chunk.len() as u64 - 3;
    }
    None
}

pub fn parse(src: &mut dyn Source, info: &mut MediaInfo) {
    let len = src.len();
    let mut m = Mp3Info::default();
    let mut start = 0u64;
    // ID3v2 at the start.
    if let Some(h) = read_array::<10>(src, 0)
        && &h[0..3] == b"ID3"
    {
        let size = 10 + syncsafe(&h[6..10]) + if h[5] & 0x10 != 0 { 10 } else { 0 };
        m.id3v2_version = Some(format!("2.{}.{}", h[3], h[4]));
        m.id3v2_size = Some(size);
        let d = src.read_upto(0, size, limits::MAX_STRUCT_BYTES);
        if (d.len() as u64) < size.min(len) {
            info.note("ID3v2 tag larger than the parser cap: only its start is read");
        }
        id3v2_from_bytes(&d, info);
        if size > len {
            info.anomaly(
                AnomalyKind::Truncated,
                0,
                format!("ID3v2 tag declares {} bytes, the file has {}", size, len),
            );
        }
        info.node(0, "ID3v2", 0, size.min(len), format!("version 2.{}", h[3]));
        start = size.min(len);
    }
    // Tags at the end: ID3v1 (last 128 bytes), then APEv2 before it or at the end.
    let mut audio_end = len;
    if len >= start + 128
        && let Some(t) = src.read_vec(len - 128, 128, 128)
        && &t[0..3] == b"TAG"
    {
        m.id3v1 = true;
        audio_end = len - 128;
        for (k, a, b) in [
            ("title", 3, 33),
            ("artist", 33, 63),
            ("album", 63, 93),
            ("year", 93, 97),
            ("comment", 97, 127),
        ] {
            info.tag("ID3v1", k, &latin1(&t[a..b]));
        }
        info.node(0, "ID3v1", len - 128, 128, String::new());
    }
    if audio_end >= start + 32
        && let Some(f) = src.read_vec(audio_end - 32, 32, 32)
        && &f[0..8] == b"APETAGEX"
    {
        m.apev2 = true;
        let size = crate::util::le32(&f, 12).unwrap_or(0) as u64;
        let header = crate::util::le32(&f, 20).unwrap_or(0) & 0x8000_0000 != 0;
        let total = size + if header { 32 } else { 0 };
        if total <= audio_end - start {
            info.node(0, "APEv2", audio_end - total, total, String::new());
            audio_end -= total;
        }
        info.tag("APEv2", "present", "yes");
    }
    m.audio_end = Some(audio_end);
    // First frame.
    let first = match read_array::<4>(src, start).and_then(|b| FrameHeader::parse(&b)) {
        Some(h) => Some((start, h)),
        None => resync(src, start, audio_end),
    };
    let Some((first_off, fh)) = first else {
        info.note("no MPEG audio frame found");
        info.logical_end = start;
        if start < audio_end {
            info.anomaly(
                AnomalyKind::SyncGap,
                start,
                format!("{} bytes without any MPEG audio frame", audio_end - start),
            );
        }
        info.mp3 = Some(m);
        return;
    };
    if first_off > start {
        info.anomaly(
            AnomalyKind::SyncGap,
            start,
            format!("{} bytes before the first frame", first_off - start),
        );
    }
    m.first_frame_offset = Some(first_off);
    m.mpeg_version = Some(match fh.version {
        1 => "MPEG-1".into(),
        2 => "MPEG-2".into(),
        _ => "MPEG-2.5".into(),
    });
    m.layer = Some(fh.layer);
    m.sample_rate = Some(fh.sample_rate);
    m.channel_mode = Some(mode_name(fh.channel_mode).into());
    let first_frame = src.read_upto(first_off, fh.length as u64, 4096);
    m.xing = xing(&first_frame, &fh);
    if let Some(x) = &m.xing
        && let Some(e) = &x.encoder
    {
        info.encoder(&format!("{} tag encoder", x.tag), e);
    }
    // Frame walk.
    let mut off = first_off;
    let (mut frames, mut samples) = (0u64, 0u64);
    let (mut min_br, mut max_br) = (u32::MAX, 0u32);
    let mut last_end = first_off;
    let mut truncated = false;
    let skip_first = m.xing.is_some();
    let mut prev = fh;
    while off + 4 <= audio_end {
        if frames >= limits::MAX_FRAMES {
            m.frames_capped = true;
            info.note(format!(
                "more than {} frames: the walk stopped (durations not compared)",
                limits::MAX_FRAMES
            ));
            break;
        }
        let h = match read_array::<4>(src, off).and_then(|b| FrameHeader::parse(&b)) {
            Some(h) => h,
            None => match resync(src, off + 1, audio_end) {
                Some((next, h)) => {
                    m.sync_gaps += 1;
                    m.skipped_bytes += next - off;
                    info.anomaly(
                        AnomalyKind::SyncGap,
                        off,
                        format!("{} bytes between frames are not MPEG audio", next - off),
                    );
                    off = next;
                    h
                }
                None => break,
            },
        };
        if h.version != prev.version
            || h.sample_rate != prev.sample_rate
            || h.layer != prev.layer
            || h.channels() != prev.channels()
        {
            info.anomaly(
                AnomalyKind::ParametersChange,
                off,
                format!(
                    "frame {}: {} Hz {} after {} Hz {}",
                    frames + 1,
                    h.sample_rate,
                    mode_name(h.channel_mode),
                    prev.sample_rate,
                    mode_name(prev.channel_mode)
                ),
            );
        }
        prev = h;
        if off + h.length as u64 > audio_end {
            truncated = true;
            info.anomaly(
                AnomalyKind::Truncated,
                off,
                format!(
                    "last frame declares {} bytes, {} are present",
                    h.length,
                    audio_end - off
                ),
            );
            last_end = audio_end;
            break;
        }
        let is_info_frame = frames == 0 && skip_first && off == first_off;
        if !is_info_frame {
            frames += 1;
            samples += h.samples as u64;
            min_br = min_br.min(h.bitrate_kbps);
            max_br = max_br.max(h.bitrate_kbps);
            if h.padding {
                m.padded_frames += 1;
            }
        }
        off += h.length as u64;
        last_end = off;
    }
    if !truncated && last_end < audio_end {
        info.anomaly(
            AnomalyKind::TrailingData,
            last_end,
            format!(
                "{} bytes after the last frame{}",
                audio_end - last_end,
                if audio_end < len {
                    " (before the end tags)"
                } else {
                    ""
                }
            ),
        );
    }
    info.logical_end = if last_end < audio_end { last_end } else { len };
    m.frames = frames;
    m.vbr = frames > 0 && min_br != max_br;
    if frames > 0 {
        m.min_bitrate_kbps = Some(min_br);
        m.max_bitrate_kbps = Some(max_br);
    }
    info.node(
        0,
        "frames",
        first_off,
        last_end - first_off,
        format!("{} frames", frames),
    );
    let ms = samples.saturating_mul(1000) / fh.sample_rate as u64;
    if !m.frames_capped && frames > 0 {
        info.duration("frames counted", ms, "computed");
    }
    if let Some(x) = &m.xing
        && let Some(n) = x.frames
    {
        info.duration(
            &format!("{} frame count", x.tag),
            n as u64 * fh.samples as u64 * 1000 / fh.sample_rate as u64,
            "declared",
        );
    }
    info.streams.push(Stream {
        index: 1,
        kind: "audio".into(),
        codec: format!("MPEG audio layer {}", fh.layer),
        sample_rate: Some(fh.sample_rate),
        channels: Some(fh.channels()),
        duration_ms: Some(ms),
        samples: Some(samples),
        bitrate_kbps: if m.vbr { None } else { Some(fh.bitrate_kbps) },
        ..Stream::default()
    });
    info.mp3 = Some(m);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_lengths() {
        // MPEG-1 layer III, 128 kb/s, 44.1 kHz, no padding: 417 bytes.
        let h = FrameHeader::parse(&[0xFF, 0xFB, 0x90, 0x64]).unwrap();
        assert_eq!(
            (h.version, h.layer, h.bitrate_kbps, h.sample_rate),
            (1, 3, 128, 44_100)
        );
        assert_eq!(h.length, 417);
        assert_eq!(h.samples, 1152);
        // Padding bit: 418 bytes.
        assert_eq!(
            FrameHeader::parse(&[0xFF, 0xFB, 0x92, 0x64])
                .unwrap()
                .length,
            418
        );
        // MPEG-2 layer III, 64 kb/s, 22.05 kHz: 72*64000/22050 = 208.
        let h = FrameHeader::parse(&[0xFF, 0xF3, 0x80, 0xC4]).unwrap();
        assert_eq!(
            (h.version, h.sample_rate, h.length, h.samples),
            (2, 22_050, 208, 576)
        );
        assert!(
            FrameHeader::parse(&[0xFF, 0xFB, 0xF0, 0x64]).is_none(),
            "bad bitrate"
        );
        assert!(
            FrameHeader::parse(&[0xFF, 0xFB, 0x0C, 0x64]).is_none(),
            "free format / reserved rate"
        );
        assert!(
            FrameHeader::parse(&[0xFF, 0xE9, 0x90, 0x64]).is_none(),
            "reserved version"
        );
    }

    #[test]
    fn id3_text_encodings() {
        assert_eq!(id3_text(b"\x00Lavf60.3.100"), "Lavf60.3.100");
        assert_eq!(id3_text(b"\x03caf\xc3\xa9\x00"), "café");
        assert_eq!(id3_text(&[1, 0xFF, 0xFE, b'a', 0, b'b', 0]), "ab");
        assert_eq!(id3_text(&[2, 0, b'a']), "a");
        assert_eq!(id3_text(b""), "");
    }
}
