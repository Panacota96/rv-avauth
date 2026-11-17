//! Ogg (RFC 3533): page walk with CRC-32 check, page sequence and granule-position continuity, logical
//! streams (BOS/EOS, chaining), and the identification + comment headers of Opus (RFC 7845) and Vorbis
//! streams (vendor string, comments). Audio packets are never decoded.

use crate::model::{AnomalyKind, MediaInfo, OggInfo, OggStream, Stream, TimeValue, limits};
use crate::source::Source;
use crate::util::{le16, le32, rfc3339};

/// Ogg CRC-32: polynomial 0x04C11DB7, MSB first, initial value 0, no final XOR.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                r = if r & 0x8000_0000 != 0 {
                    (r << 1) ^ 0x04C1_1DB7
                } else {
                    r << 1
                };
            }
            *e = r;
        }
        t
    });
    data.iter().fold(0u32, |crc, &b| {
        (crc << 8) ^ t[(((crc >> 24) as u8) ^ b) as usize]
    })
}

const PACKET_CAP: usize = 256 * 1024;

struct Track {
    s: OggStream,
    last_seq: Option<u32>,
    packets: Vec<Vec<u8>>,
    partial: Vec<u8>,
    ended: bool,
}

/// Position of the next "OggS" at or after `from`, scanning at most `MAX_RESYNC_SCAN` bytes.
fn resync(src: &mut dyn Source, from: u64) -> Option<u64> {
    let mut at = from;
    let stop = from.saturating_add(limits::MAX_RESYNC_SCAN).min(src.len());
    while at < stop {
        let chunk = src.read_upto(at, 64 * 1024 + 3, 64 * 1024 + 3);
        if chunk.len() < 4 {
            return None;
        }
        if let Some(i) = chunk.windows(4).position(|w| w == b"OggS") {
            return Some(at + i as u64);
        }
        at += chunk.len() as u64 - 3;
    }
    None
}

pub fn parse(src: &mut dyn Source, info: &mut MediaInfo) {
    let len = src.len();
    let mut og = OggInfo::default();
    let mut tracks: Vec<Track> = vec![];
    let mut off = 0u64;
    let mut cut = false;
    while off < len {
        if og.pages >= limits::MAX_PAGES {
            info.anomaly(
                AnomalyKind::LimitReached,
                off,
                format!(
                    "more than {} pages: the rest is not walked",
                    limits::MAX_PAGES
                ),
            );
            off = len;
            break;
        }
        let h = src.read_upto(off, 27, 27);
        if h.len() < 27 {
            if !h.is_empty() && b"OggS".starts_with(&h[..h.len().min(4)]) {
                info.anomaly(
                    AnomalyKind::Truncated,
                    off,
                    format!("last page header cut after {} bytes", h.len()),
                );
                cut = true;
            }
            break;
        }
        if &h[0..4] != b"OggS" || h[4] != 0 {
            match resync(src, off + 1) {
                Some(next) => {
                    info.anomaly(
                        AnomalyKind::SyncGap,
                        off,
                        format!("{} bytes that are not an Ogg page", next - off),
                    );
                    off = next;
                    continue;
                }
                None => break,
            }
        }
        let htype = h[5];
        let granule = i64::from_le_bytes(h[6..14].try_into().unwrap_or([0; 8]));
        let serial = le32(&h, 14).unwrap_or(0);
        let seq = le32(&h, 18).unwrap_or(0);
        let crc = le32(&h, 22).unwrap_or(0);
        let nseg = h[26] as u64;
        let Some(segs) = src.read_vec(off + 27, nseg, 255) else {
            info.anomaly(
                AnomalyKind::Truncated,
                off,
                "last page cut inside its segment table",
            );
            cut = true;
            break;
        };
        let body: u64 = segs.iter().map(|&s| s as u64).sum();
        let page_len = 27 + nseg + body;
        let Some(mut page) = src.read_vec(off, page_len, 65_307) else {
            info.anomaly(
                AnomalyKind::Truncated,
                off,
                format!(
                    "last page declares {} bytes, {} are present",
                    page_len,
                    len - off
                ),
            );
            cut = true;
            break;
        };
        page[22..26].fill(0);
        let crc_ok = crc32(&page) == crc;
        // Stream bookkeeping.
        let idx = match tracks.iter().position(|t| t.s.serial == serial && !t.ended) {
            Some(i) => i,
            None => {
                if htype & 2 == 0 {
                    info.note(format!(
                        "page of stream {:08x} at offset {} without a beginning-of-stream page",
                        serial, off
                    ));
                } else if !tracks.is_empty() && tracks.iter().all(|t| t.ended) {
                    og.chained = true;
                    info.anomaly(
                        AnomalyKind::ChainedStreams,
                        off,
                        format!(
                            "logical stream {:08x} starts after the previous stream ended (chained Ogg)",
                            serial
                        ),
                    );
                }
                tracks.push(Track {
                    s: OggStream {
                        serial,
                        bos: htype & 2 != 0,
                        first_page_offset: off,
                        ..OggStream::default()
                    },
                    last_seq: None,
                    packets: vec![],
                    partial: vec![],
                    ended: false,
                });
                tracks.len() - 1
            }
        };
        let t = &mut tracks[idx];
        t.s.pages += 1;
        if !crc_ok {
            t.s.crc_failures += 1;
            og.crc_failures += 1;
            info.anomaly(
                AnomalyKind::CrcMismatch,
                off,
                format!(
                    "page {} of stream {:08x}: CRC-32 does not match",
                    seq, serial
                ),
            );
        }
        if let Some(last) = t.last_seq
            && seq != last.wrapping_add(1)
        {
            t.s.sequence_gaps += 1;
            info.anomaly(
                AnomalyKind::SequenceGap,
                off,
                format!(
                    "stream {:08x}: page sequence {} follows {}",
                    serial, seq, last
                ),
            );
        }
        t.last_seq = Some(seq);
        if granule != -1 {
            if let Some(lg) = t.s.last_granule
                && granule < lg
            {
                info.anomaly(
                    AnomalyKind::GranuleNotMonotonic,
                    off,
                    format!(
                        "stream {:08x}: granule position {} after {}",
                        serial, granule, lg
                    ),
                );
            }
            t.s.first_granule.get_or_insert(granule);
            t.s.last_granule = Some(granule);
        }
        // The first two packets (identification + comment headers).
        if t.packets.len() < 2 {
            if htype & 1 == 0 {
                t.partial.clear();
            }
            let mut p = 27 + nseg as usize;
            for &sl in &segs {
                let sl = sl as usize;
                if t.packets.len() >= 2 {
                    break;
                }
                if t.partial.len() < PACKET_CAP {
                    let take = sl.min(PACKET_CAP - t.partial.len());
                    t.partial.extend_from_slice(&page[p..p + take]);
                }
                p += sl;
                if sl < 255 {
                    let done = std::mem::take(&mut t.partial);
                    t.packets.push(done);
                }
            }
        }
        if htype & 4 != 0 {
            t.s.eos = true;
            t.ended = true;
        }
        info.node(
            1,
            "page",
            off,
            page_len,
            format!(
                "stream {:08x} seq {} granule {}{}{}",
                serial,
                seq,
                granule,
                if htype & 2 != 0 { " BOS" } else { "" },
                if htype & 4 != 0 { " EOS" } else { "" }
            ),
        );
        og.pages += 1;
        off += page_len;
    }
    info.logical_end = if cut { len } else { off.min(len) };
    if !cut && off < len {
        info.anomaly(
            AnomalyKind::TrailingData,
            off,
            format!("{} bytes after the last Ogg page", len - off),
        );
    }
    for (i, t) in tracks.into_iter().enumerate() {
        let mut s = t.s;
        headers(info, &mut s, &t.packets);
        if !s.eos {
            info.anomaly(
                AnomalyKind::Truncated,
                info.logical_end,
                format!("stream {:08x} has no end-of-stream page", s.serial),
            );
        }
        let mut st = Stream {
            index: i as u32 + 1,
            kind: if s.codec == "Theora" {
                "video"
            } else {
                "audio"
            }
            .into(),
            codec: s.codec.clone(),
            sample_rate: s.sample_rate,
            channels: s.channels,
            ..Stream::default()
        };
        if let (Some(g), Some(rate)) = (s.last_granule, s.sample_rate)
            && g > 0
            && rate > 0
            && s.codec != "Theora"
        {
            // Opus granules count 48 kHz samples including the pre-skip.
            let samples = (g as u64).saturating_sub(s.pre_skip.unwrap_or(0) as u64);
            let ms = samples.saturating_mul(1000)
                / if s.codec == "Opus" {
                    48_000
                } else {
                    rate as u64
                };
            st.duration_ms = Some(ms);
            st.samples = Some(samples);
            info.duration(&format!("last granule [{:08x}]", s.serial), ms, "computed");
        }
        info.streams.push(st);
        og.streams.push(s);
    }
    info.ogg = Some(og);
}

fn headers(info: &mut MediaInfo, s: &mut OggStream, packets: &[Vec<u8>]) {
    let Some(id) = packets.first() else {
        s.codec = "unknown".into();
        return;
    };
    let comments: Option<(&str, usize)> = if id.starts_with(b"OpusHead") {
        s.codec = "Opus".into();
        s.channels = id.get(9).map(|&c| c as u16);
        s.pre_skip = le16(id, 10);
        s.sample_rate = le32(id, 12).filter(|r| *r > 0).or(Some(48_000));
        Some(("OpusTags", 8))
    } else if id.starts_with(b"\x01vorbis") {
        s.codec = "Vorbis".into();
        s.channels = id.get(11).map(|&c| c as u16);
        s.sample_rate = le32(id, 12);
        Some(("\x03vorbis", 7))
    } else if id.starts_with(b"\x80theora") {
        s.codec = "Theora".into();
        None
    } else if id.starts_with(b"Speex   ") {
        s.codec = "Speex".into();
        s.sample_rate = le32(id, 36);
        Some(("", 0))
    } else if id.starts_with(b"\x7FFLAC") {
        s.codec = "FLAC".into();
        None
    } else {
        s.codec = "unknown".into();
        None
    };
    let Some((magic, at)) = comments else { return };
    let Some(c) = packets.get(1) else { return };
    if !magic.is_empty() && !c.starts_with(magic.as_bytes()) {
        return;
    }
    let source = if s.codec == "Opus" {
        "OpusTags"
    } else {
        "VorbisComment"
    };
    let Some(vl) = le32(c, at) else { return };
    let mut p = at + 4;
    let Some(v) = c.get(p..p.saturating_add(vl as usize)) else {
        info.anomaly(
            AnomalyKind::SizeInconsistent,
            0,
            format!("{} vendor length {} runs past the packet", source, vl),
        );
        return;
    };
    let vendor = crate::util::clean_text(&crate::util::text(v));
    if !vendor.is_empty() {
        s.vendor = Some(vendor.clone());
        info.encoder(&format!("{} vendor", source), &vendor);
    }
    p += vl as usize;
    let n = le32(c, p).unwrap_or(0);
    p += 4;
    for _ in 0..n.min(1024) {
        let Some(l) = le32(c, p) else { break };
        let Some(kv) = c.get(p + 4..(p + 4).saturating_add(l as usize)) else {
            break;
        };
        p += 4 + l as usize;
        let kv = crate::util::text(kv);
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        let k = k.to_ascii_uppercase();
        if k == "METADATA_BLOCK_PICTURE" {
            info.tag(source, &k, &format!("[picture, {} bytes]", v.len()));
            continue;
        }
        info.tag(source, &k, v);
        match k.as_str() {
            "ENCODER" | "ENCODED_BY" | "ENCODER_OPTIONS" | "SOFTWARE" | "TOOL" => {
                info.encoder(&format!("{} {}", source, k), v)
            }
            "DATE" | "CREATION_TIME" => {
                let pd = crate::util::parse_tag_date(v);
                info.time(TimeValue {
                    source: format!("{} {}", source, k),
                    raw: crate::util::clean_text(v),
                    unix: pd.map(|x| x.0),
                    utc: pd.and_then(|x| rfc3339(x.0)),
                    origin: if pd.is_some_and(|x| x.1) {
                        "tag"
                    } else {
                        "tag_local"
                    }
                    .into(),
                });
            }
            "MAKE" | "MODEL" | "DEVICE" => {
                let v = crate::util::clean_text(v);
                if !v.is_empty() {
                    info.device_claims.push(crate::model::Tag {
                        source: source.into(),
                        key: k.clone(),
                        value: v,
                    });
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc_matches_the_ogg_reference_value() {
        // CRC of "OggS" + zeros computed with the reference algorithm (libogg).
        assert_eq!(super::crc32(b""), 0);
        assert_eq!(super::crc32(b"123456789"), 0x89A1_897F);
    }
}
