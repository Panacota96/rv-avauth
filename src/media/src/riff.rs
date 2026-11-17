//! RIFF/WAVE (and RF64): chunk walk with size checks, `fmt `, `data`, `fact`, `LIST`/`INFO`, `bext`
//! (EBU Tech 3285 broadcast extension), `ds64` (RF64), embedded `id3 ` chunks. Audio samples are never
//! read.

use crate::model::{AnomalyKind, MediaInfo, Stream, TimeValue, WavInfo, limits};
use crate::source::{Source, read_array};
use crate::util::{fourcc, latin1, le16, le32, le64, rfc3339};

pub fn parse(src: &mut dyn Source, info: &mut MediaInfo) {
    let len = src.len();
    let mut wav = WavInfo::default();
    let Some(h) = read_array::<12>(src, 0) else {
        info.anomaly(
            AnomalyKind::Truncated,
            0,
            "RIFF header shorter than 12 bytes",
        );
        info.wav = Some(wav);
        return;
    };
    wav.rf64 = &h[0..4] == b"RF64";
    let mut riff_size = le32(&h, 4).unwrap_or(0) as u64;
    info.node(
        0,
        &fourcc(&h[0..4]),
        0,
        len,
        format!("form WAVE, size field {}", riff_size),
    );
    let mut ds64_data: Option<u64> = None;
    let mut off = 12u64;
    if wav.rf64 {
        // ds64 must follow: riffSize (8), dataSize (8), sampleCount (8), table…
        if let Some(c) = src.read_vec(12, 36, 36)
            && &c[0..4] == b"ds64"
        {
            riff_size = le64(&c, 8).unwrap_or(riff_size);
            ds64_data = le64(&c, 16);
            wav.fact_samples = le64(&c, 24);
        }
    }
    wav.riff_size = riff_size;
    let declared_end = riff_size.saturating_add(8);
    if declared_end > len {
        info.anomaly(
            AnomalyKind::Truncated,
            0,
            format!(
                "RIFF size field says the file has {} bytes; it has {}",
                declared_end, len
            ),
        );
    }
    let end = declared_end.min(len);
    let mut chunks = 0usize;
    while off + 8 <= end {
        chunks += 1;
        if chunks > limits::MAX_CHUNKS {
            info.anomaly(
                AnomalyKind::LimitReached,
                off,
                "more chunks than the parser walks",
            );
            break;
        }
        let Some(ch) = read_array::<8>(src, off) else {
            break;
        };
        let id = fourcc(&ch[0..4]);
        if !crate::util::plausible_fourcc(&ch[0..4]) {
            info.anomaly(
                AnomalyKind::SizeInconsistent,
                off,
                format!("invalid chunk id at offset {} inside the RIFF form", off),
            );
            break;
        }
        let mut size = le32(&ch, 4).unwrap_or(0) as u64;
        if wav.rf64 && &ch[0..4] == b"data" && size == 0xFFFF_FFFF {
            size = ds64_data.unwrap_or(size);
        }
        let payload = off + 8;
        let avail = end.saturating_sub(payload);
        let mut stop = false;
        if size > avail {
            let kind = if payload + size > len {
                AnomalyKind::Truncated
            } else {
                AnomalyKind::SizeInconsistent
            };
            info.anomaly(
                kind,
                off,
                format!(
                    "chunk '{}' declares {} bytes, {} are inside the RIFF form",
                    id, size, avail
                ),
            );
            stop = true;
        }
        let present = size.min(avail);
        info.node(1, &id, off, 8 + present, String::new());
        if wav.chunks.len() < 200 {
            wav.chunks.push(id.clone());
        }
        match &ch[0..4] {
            b"fmt " => fmt(src, info, &mut wav, payload, present),
            b"data" => {
                wav.data_offset = Some(payload);
                wav.data_size_declared = Some(size);
                wav.data_size_present = Some(present);
            }
            b"fact" => {
                if let Some(d) = src.read_vec(payload, 4.min(present), 4)
                    && !wav.rf64
                {
                    wav.fact_samples = le32(&d, 0).map(|x| x as u64);
                }
            }
            b"LIST" => list(src, info, payload, present),
            b"bext" => bext(src, info, payload, present),
            b"id3 " | b"ID3 " => {
                // An ID3v2 tag inside the WAVE file (written by some editors).
                let d = src.read_upto(payload, present, limits::MAX_STRUCT_BYTES);
                crate::mp3::id3v2_from_bytes(&d, info);
            }
            b"iXML" => {
                let d = src.read_upto(payload, present, 256 * 1024);
                let s = crate::util::text(&d);
                for k in ["<PROJECT>", "<NOTE>"] {
                    if let Some(i) = s.find(k)
                        && let Some(v) = s[i + k.len()..].split('<').next()
                    {
                        info.tag("iXML", k.trim_matches(|c| c == '<' || c == '>'), v);
                    }
                }
            }
            _ => {}
        }
        if stop {
            off = end;
            break;
        }
        // Chunks are padded to an even size.
        off = payload + size + (size & 1);
    }
    if off == end + 1 {
        // Missing pad byte after the last odd-sized chunk: tolerated by every reader, noted only.
        info.note("the last chunk has an odd size and no pad byte");
    }
    info.logical_end = off.min(end);
    if declared_end < len {
        info.logical_end = declared_end;
        info.anomaly(
            AnomalyKind::TrailingData,
            declared_end,
            format!(
                "{} bytes after the end of the RIFF form (size field {})",
                len - declared_end,
                riff_size
            ),
        );
    }
    // Stream and durations.
    let mut s = Stream {
        index: 1,
        kind: "audio".into(),
        codec: wav
            .format_tag
            .map(format_name)
            .unwrap_or_else(|| "unknown".into()),
        sample_rate: wav.sample_rate,
        channels: wav.channels,
        bits_per_sample: wav.bits_per_sample,
        bitrate_kbps: wav.byte_rate.map(|b| (b as u64 * 8 / 1000) as u32),
        ..Stream::default()
    };
    if let (Some(br), Some(decl)) = (wav.byte_rate, wav.data_size_declared)
        && br > 0
    {
        let declared = decl as u128 * 1000 / br as u128;
        s.duration_ms = Some(declared as u64);
        info.duration("data chunk size field", declared as u64, "declared");
        if let Some(p) = wav.data_size_present {
            info.duration(
                "data bytes present",
                (p as u128 * 1000 / br as u128) as u64,
                "computed",
            );
        }
        if let Some(ba) = wav.block_align
            && ba > 0
            && let Some(p) = wav.data_size_present
        {
            s.samples = Some(p / ba as u64);
            if p % ba as u64 != 0
                && wav
                    .format_tag
                    .is_some_and(|f| f == 1 || f == 3 || f == 0xFFFE)
            {
                info.anomaly(
                    AnomalyKind::Truncated,
                    wav.data_offset.unwrap_or(0) + p,
                    format!(
                        "data chunk holds {} bytes, not a whole number of {}-byte sample frames",
                        p, ba
                    ),
                );
            }
        }
    }
    if let (Some(fs), Some(sr)) = (wav.fact_samples, wav.sample_rate)
        && sr > 0
        && wav.format_tag.is_some_and(|f| f != 1)
    {
        info.duration(
            "fact sample count",
            fs.saturating_mul(1000) / sr as u64,
            "declared",
        );
    }
    if wav.format_tag.is_some() {
        info.streams.push(s);
    } else {
        info.note("no fmt chunk: format unknown");
    }
    info.wav = Some(wav);
}

fn format_name(tag: u16) -> String {
    match tag {
        1 => "PCM".into(),
        2 => "MS ADPCM".into(),
        3 => "IEEE float".into(),
        6 => "A-law".into(),
        7 => "mu-law".into(),
        0x11 => "IMA ADPCM".into(),
        0x31 => "GSM 6.10".into(),
        0x50 => "MPEG".into(),
        0x55 => "MPEG layer III".into(),
        0xFFFE => "extensible".into(),
        t => format!("0x{:04X}", t),
    }
}

fn fmt(src: &mut dyn Source, info: &mut MediaInfo, wav: &mut WavInfo, payload: u64, n: u64) {
    let Some(d) = src.read_vec(payload, n.min(40), 40) else {
        return;
    };
    if d.len() < 16 {
        info.anomaly(
            AnomalyKind::SizeInconsistent,
            payload,
            format!("fmt chunk of {} bytes (at least 16 expected)", d.len()),
        );
        return;
    }
    let mut tag = le16(&d, 0).unwrap_or(0);
    let ch = le16(&d, 2).unwrap_or(0);
    let sr = le32(&d, 4).unwrap_or(0);
    let br = le32(&d, 8).unwrap_or(0);
    let ba = le16(&d, 12).unwrap_or(0);
    let bits = le16(&d, 14).unwrap_or(0);
    if tag == 0xFFFE
        && let Some(sub) = le16(&d, 24)
    {
        // WAVE_FORMAT_EXTENSIBLE: the sub-format GUID starts with the real format tag.
        tag = if sub == 1 || sub == 3 { sub } else { 0xFFFE };
    }
    wav.format_tag = Some(tag);
    wav.channels = Some(ch);
    wav.sample_rate = Some(sr);
    wav.byte_rate = Some(br);
    wav.block_align = Some(ba);
    wav.bits_per_sample = Some(bits);
    if tag == 1 || tag == 3 {
        let expect_ba = ch as u32 * bits.div_ceil(8) as u32;
        if ba as u32 != expect_ba {
            info.anomaly(
                AnomalyKind::FormatParameters,
                payload,
                format!(
                    "block align {} but {} channels x {} bits need {}",
                    ba, ch, bits, expect_ba
                ),
            );
        }
        if br as u64 != sr as u64 * ba as u64 {
            info.anomaly(
                AnomalyKind::FormatParameters,
                payload,
                format!(
                    "byte rate {} but sample rate {} x block align {} = {}",
                    br,
                    sr,
                    ba,
                    sr as u64 * ba as u64
                ),
            );
        }
    }
}

fn list(src: &mut dyn Source, info: &mut MediaInfo, payload: u64, n: u64) {
    let d = src.read_upto(payload, n, limits::MAX_STRUCT_BYTES);
    if d.get(0..4) != Some(b"INFO") {
        return;
    }
    let mut at = 4usize;
    let mut items = 0;
    while at + 8 <= d.len() && items < 512 {
        items += 1;
        let id = fourcc(&d[at..at + 4]);
        let sz = le32(&d, at + 4).unwrap_or(0) as usize;
        if at + 8 + sz > d.len() {
            info.anomaly(
                AnomalyKind::SizeInconsistent,
                payload + at as u64,
                format!("INFO item '{}' runs past its LIST chunk", id),
            );
            break;
        }
        let v = crate::util::text(&d[at + 8..at + 8 + sz]);
        info.tag("INFO", &id, &v);
        match id.as_str() {
            "ISFT" => info.encoder("INFO ISFT", &v),
            "ICRD" => {
                let p = crate::util::parse_tag_date(&v);
                info.time(TimeValue {
                    source: "INFO ICRD".into(),
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
            "IENG" | "ITCH" => {}
            _ => {}
        }
        at += 8 + sz + (sz & 1);
    }
}

/// Broadcast Wave `bext`: Description (256), Originator (32), OriginatorReference (32), OriginationDate
/// (10), OriginationTime (8), TimeReference (8), Version (2), UMID (64), loudness (10), reserved (180),
/// CodingHistory (rest).
fn bext(src: &mut dyn Source, info: &mut MediaInfo, payload: u64, n: u64) {
    let d = src.read_upto(payload, n, 64 * 1024);
    if d.len() < 346 {
        info.anomaly(
            AnomalyKind::SizeInconsistent,
            payload,
            format!("bext chunk of {} bytes (at least 346 expected)", d.len()),
        );
        return;
    }
    let field = |a: usize, b: usize| crate::util::clean_text(&latin1(&d[a..b]));
    let desc = field(0, 256);
    let orig = field(256, 288);
    let oref = field(288, 320);
    let date = field(320, 330);
    let time = field(330, 338);
    info.tag("bext", "Description", &desc);
    info.tag("bext", "Originator", &orig);
    info.tag("bext", "OriginatorReference", &oref);
    info.tag("bext", "OriginationDate", &date);
    info.tag("bext", "OriginationTime", &time);
    if !orig.is_empty() {
        info.encoder("bext Originator", &orig);
        info.device_claims.push(crate::model::Tag {
            source: "bext".into(),
            key: "Originator".into(),
            value: orig.clone(),
        });
    }
    if d.len() > 602 {
        let hist = field(602, d.len());
        if !hist.is_empty() {
            info.tag("bext", "CodingHistory", &hist);
            info.encoder("bext CodingHistory", &hist);
        }
    }
    if !date.is_empty() {
        let joined = if time.is_empty() {
            date.clone()
        } else {
            format!("{} {}", date, time.replace(['-', '.'], ":"))
        };
        let p = crate::util::parse_tag_date(&joined.replace('/', "-"));
        info.time(TimeValue {
            source: "bext OriginationDate".into(),
            raw: joined,
            unix: p.map(|x| x.0),
            utc: p.and_then(|x| rfc3339(x.0)),
            origin: "tag_local".into(),
        });
    }
}
