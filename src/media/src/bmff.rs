//! ISO base media file format (ISO/IEC 14496-12) and QuickTime: MP4, MOV, M4A, 3GP.
//!
//! Walks the box tree (sizes checked against the parent and the file, nesting ≤ `MAX_DEPTH`), reads `ftyp`,
//! `mvhd`/`tkhd`/`mdhd` times and durations, `hdlr`, the first `stsd` sample entry (codec fourcc, audio
//! and video parameters, vendor, compressor name), `stts`/`stsz` sample tables, `udta`, `meta` (ISO and
//! QuickTime styles, `keys` + `ilst`), and records `mdat`, `moof`, `free`/`skip`/`wide`, `uuid` boxes.
//! `mdat` payloads are never read.

use crate::model::{
    AnomalyKind, BmffInfo, Brand, MediaInfo, Stream, TimeValue, limits::MAX_DEPTH,
    limits::MAX_STRUCT_BYTES, limits::MAX_TABLE_ENTRIES,
};
use crate::source::{Source, read_array};
use crate::util::{be16, be32, be64, fourcc, mac_to_unix, plausible_fourcc, rfc3339, text};

/// Boxes whose payload is a sequence of boxes.
const CONTAINERS: &[&[u8; 4]] = &[
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"udta", b"dinf", b"mvex", b"moof",
    b"traf", b"mfra", b"sinf", b"schi", b"tref", b"gmhd", b"tapt", b"clip", b"matt",
];

struct Walk {
    b: BmffInfo,
    /// Boxes walked in one parent before the walk stops (`MAX_BOXES`; tests use a small one).
    max_boxes: u32,
    /// Set when a parent was not walked to its end because of `max_boxes`.
    capped: bool,
    /// Index into `info.streams` of the track being read.
    track: Option<usize>,
    /// `keys` of the current QuickTime `meta` (ilst items refer to them by 1-based index).
    keys: Vec<String>,
    tracks_seen: u32,
}

/// Boxes walked in one parent.
pub const MAX_BOXES: u32 = 100_000;

pub fn parse(src: &mut dyn Source, info: &mut MediaInfo) {
    parse_with_limit(src, info, MAX_BOXES)
}

/// `parse` with an explicit cap on boxes per parent.
pub fn parse_with_limit(src: &mut dyn Source, info: &mut MediaInfo, max_boxes: u32) {
    let mut w = Walk {
        b: BmffInfo::default(),
        max_boxes,
        capped: false,
        track: None,
        keys: vec![],
        tracks_seen: 0,
    };
    let len = src.len();
    let end = walk(src, info, &mut w, 0, len, 0, b"\0\0\0\0");
    // A capped walk leaves the rest of the file unexamined (PARSER_LIMIT_REACHED), not trailing data.
    info.logical_end = if w.capped { len } else { end };
    if end < len && !w.capped {
        let n = len - end;
        let sample = src.read_upto(end, n, 1024 * 1024);
        let zero = sample.iter().all(|&c| c == 0) && n <= 1024 * 1024;
        info.anomaly(
            AnomalyKind::TrailingData,
            end,
            format!(
                "{} bytes after the last complete top-level box{}",
                n,
                if zero { " (all zero)" } else { "" }
            ),
        );
    }
    finish(info, &mut w);
    info.bmff = Some(w.b);
}

/// Walks the boxes in [start, end); returns where the last valid box ended.
#[allow(clippy::too_many_arguments)]
fn walk(
    src: &mut dyn Source,
    info: &mut MediaInfo,
    w: &mut Walk,
    start: u64,
    end: u64,
    depth: usize,
    parent: &[u8; 4],
) -> u64 {
    let top = depth == 0;
    let file_len = src.len();
    let mut off = start;
    let mut boxes = 0u32;
    while off < end {
        boxes += 1;
        if boxes > w.max_boxes {
            w.capped = true;
            info.anomaly(
                AnomalyKind::LimitReached,
                off,
                "more than 100000 boxes in one parent: the rest is not walked",
            );
            return off;
        }
        if end - off < 8 {
            if !top {
                info.anomaly(
                    AnomalyKind::SizeInconsistent,
                    off,
                    format!(
                        "{} stray bytes at the end of '{}' (too short for a box header)",
                        end - off,
                        fourcc(parent)
                    ),
                );
            }
            return off;
        }
        let Some(h) = read_array::<16>(src, off).or_else(|| {
            let v = src.read_upto(off, 16, 16);
            let mut a = [0u8; 16];
            a[..v.len()].copy_from_slice(&v);
            Some(a)
        }) else {
            return off;
        };
        let size32 = be32(&h, 0).unwrap_or(0) as u64;
        let typ: [u8; 4] = [h[4], h[5], h[6], h[7]];
        let (mut size, mut hdr) = (size32, 8u64);
        if size32 == 1 {
            size = be64(&h, 8).unwrap_or(0);
            hdr = 16;
        } else if size32 == 0 {
            size = end - off;
        }
        if &typ == b"uuid" {
            hdr += 16;
        }
        if !plausible_fourcc(&typ) || size < hdr {
            if top {
                // Not a box: the bytes from here are reported as trailing data by the caller.
                return off;
            }
            info.anomaly(
                AnomalyKind::SizeInconsistent,
                off,
                format!(
                    "invalid box header inside '{}' (type {}, size {})",
                    fourcc(parent),
                    fourcc(&typ),
                    size
                ),
            );
            return off;
        }
        let name = fourcc(&typ);
        let declared_end = off.saturating_add(size);
        let mut box_end = declared_end;
        let mut stop_after = false;
        if declared_end > end {
            if top || declared_end > file_len {
                info.anomaly(
                    AnomalyKind::Truncated,
                    off,
                    format!(
                        "box '{}' declares {} bytes but only {} are present",
                        name,
                        size,
                        end - off
                    ),
                );
            } else {
                info.anomaly(
                    AnomalyKind::SizeInconsistent,
                    off,
                    format!(
                        "box '{}' ({} bytes) ends {} bytes past its parent '{}'",
                        name,
                        size,
                        declared_end - end,
                        fourcc(parent)
                    ),
                );
            }
            box_end = end;
            stop_after = true;
        }
        let payload = off + hdr;
        let plen = box_end.saturating_sub(payload);
        let mut note = String::new();
        if &typ == b"uuid"
            && let Some(u) = src.read_vec(off + hdr - 16, 16, 16)
        {
            let hex: String = u.iter().map(|c| format!("{:02x}", c)).collect();
            note = format!("extended type {}", hex);
            if w.b
                .vendor_traces
                .iter()
                .filter(|t| t.starts_with("uuid:"))
                .count()
                < 20
            {
                w.b.vendor_traces.push(format!("uuid:{}", hex));
            }
        }
        if top {
            w.b.top_level.push(name.clone());
        }
        info.node(depth, &name, off, box_end - off, note);
        box_payload(src, info, w, &typ, payload, plen, depth, parent);
        off = box_end;
        if stop_after {
            return off;
        }
    }
    off
}

#[allow(clippy::too_many_arguments)]
fn box_payload(
    src: &mut dyn Source,
    info: &mut MediaInfo,
    w: &mut Walk,
    typ: &[u8; 4],
    payload: u64,
    plen: u64,
    depth: usize,
    parent: &[u8; 4],
) {
    let recurse = |src: &mut dyn Source, info: &mut MediaInfo, w: &mut Walk, start: u64| {
        if depth + 1 >= MAX_DEPTH {
            info.anomaly(
                AnomalyKind::LimitReached,
                start,
                format!(
                    "boxes nested deeper than {} levels are not walked",
                    MAX_DEPTH
                ),
            );
            return;
        }
        walk(src, info, w, start, payload + plen, depth + 1, typ);
    };
    match typ {
        b"trak" => {
            w.tracks_seen += 1;
            info.streams.push(Stream {
                index: w.tracks_seen,
                kind: "other".into(),
                ..Stream::default()
            });
            w.track = Some(info.streams.len() - 1);
            recurse(src, info, w, payload);
            w.track = None;
        }
        b"moov" => {
            w.b.moov_count += 1;
            if w.b.moov_offset.is_none() {
                w.b.moov_offset = Some(payload);
            }
            recurse(src, info, w, payload);
        }
        b"moof" => {
            w.b.fragmented = true;
            recurse(src, info, w, payload);
        }
        b"mdat" => w.b.mdat.push((payload, plen)),
        b"free" | b"skip" | b"wide" => {
            w.b.free_boxes += 1;
            w.b.free_bytes += plen;
        }
        b"meta" => {
            // ISO meta is a full box (4 bytes version/flags); QuickTime meta is not.
            let probe = src.read_upto(payload, 8, 8);
            let qt = probe.get(4..8) == Some(b"hdlr") || probe.get(4..8) == Some(b"keys");
            w.keys.clear();
            recurse(src, info, w, if qt { payload } else { payload + 4 });
        }
        b"ilst" => ilst(src, info, w, payload, plen),
        b"keys" => keys(src, info, w, payload, plen),
        b"ftyp" => {
            if let Some(d) = src.read_vec(payload, plen.min(4 + 4 + 4 * 64), MAX_STRUCT_BYTES)
                && d.len() >= 8
            {
                info.brand = Some(Brand {
                    major: fourcc(&d[0..4]),
                    minor_version: be32(&d, 4).unwrap_or(0),
                    compatible: d[8..]
                        .chunks_exact(4)
                        .map(fourcc)
                        .filter(|s| !s.chars().all(|c| c == ' ' || c == '0'))
                        .collect(),
                });
            }
        }
        b"mvhd" => mvhd(src, info, w, payload, plen),
        b"tkhd" => tkhd(src, info, w, payload, plen),
        b"mdhd" => mdhd(src, info, w, payload, plen),
        b"hdlr" => hdlr(src, info, w, payload, plen, parent),
        b"stsd" => stsd(src, info, w, payload, plen),
        b"stts" => stts(src, info, w, payload, plen),
        b"stsz" => {
            if let (Some(t), Some(d)) = (w.track, src.read_vec(payload, 12.min(plen), 12))
                && let Some(n) = be32(&d, 8)
            {
                info.streams[t].samples.get_or_insert(n as u64);
            }
        }
        b"elst" => w.b.edit_lists += 1,
        b"smta" => w.b.vendor_traces.push("samsung_smta".into()),
        b"loci" => w.b.vendor_traces.push("3gpp_location".into()),
        b"Xtra" => w.b.vendor_traces.push("microsoft_xtra".into()),
        b"XMP_" => {
            w.b.vendor_traces.push("xmp_packet".into());
            let d = src.read_upto(payload, plen, 256 * 1024);
            let s = text(&d);
            for key in ["xmp:CreatorTool", "CreatorTool"] {
                if let Some(v) = xmp_value(&s, key) {
                    info.tag("XMP", "CreatorTool", &v);
                    info.encoder("XMP CreatorTool", &v);
                    break;
                }
            }
        }
        _ if CONTAINERS.contains(&typ) => recurse(src, info, w, payload),
        _ if typ[0] == 0xA9 && parent == b"udta" => udta_text(src, info, typ, payload, plen),
        b"auth" | b"titl" | b"dscp" | b"cprt" | b"perf" | b"gnre" | b"albm"
            if parent == b"udta" =>
        {
            // 3GPP asset boxes: version/flags (4), language (2), string.
            let d = src.read_upto(payload, plen, 64 * 1024);
            if d.len() > 6 {
                info.tag("udta", &fourcc(typ), &text(&d[6..]));
            }
        }
        _ => {}
    }
}

/// Value of an XMP attribute or element (first match), for CreatorTool.
fn xmp_value(s: &str, key: &str) -> Option<String> {
    let i = s.find(key)?;
    let rest = &s[i + key.len()..];
    if let Some(r) = rest.strip_prefix("=\"") {
        return r.split('"').next().map(|v| v.to_string());
    }
    if let Some(r) = rest.strip_prefix('>') {
        return r.split('<').next().map(|v| v.to_string());
    }
    None
}

fn version_times(d: &[u8]) -> Option<(u8, u64, u64, usize)> {
    let v = *d.first()?;
    if v == 1 {
        Some((v, be64(d, 4)?, be64(d, 12)?, 20))
    } else {
        Some((v, be32(d, 4)? as u64, be32(d, 8)? as u64, 12))
    }
}

fn push_time(info: &mut MediaInfo, source: String, raw: u64) {
    let unix = mac_to_unix(raw);
    info.time(TimeValue {
        source,
        raw: raw.to_string(),
        unix,
        utc: unix.and_then(rfc3339),
        origin: "header".into(),
    });
}

fn ms(duration: u64, timescale: u32) -> Option<u64> {
    if timescale == 0 {
        return None;
    }
    Some(((duration as u128 * 1000) / timescale as u128).min(u64::MAX as u128) as u64)
}

fn mvhd(src: &mut dyn Source, info: &mut MediaInfo, w: &mut Walk, payload: u64, plen: u64) {
    let Some(d) = src.read_vec(payload, plen.min(32), 32) else {
        return;
    };
    let Some((v, c, m, at)) = version_times(&d) else {
        return;
    };
    let ts = be32(&d, at).unwrap_or(0);
    let dur = if v == 1 {
        be64(&d, at + 4)
    } else {
        be32(&d, at + 4).map(|x| x as u64)
    }
    .unwrap_or(0);
    push_time(info, "mvhd.creation".into(), c);
    push_time(info, "mvhd.modification".into(), m);
    w.b.movie_timescale = Some(ts);
    w.b.movie_duration = Some(dur);
    if let Some(x) = ms(dur, ts)
        && dur != u32::MAX as u64
        && dur != u64::MAX
    {
        info.duration("mvhd", x, "declared");
    }
}

fn tkhd(src: &mut dyn Source, info: &mut MediaInfo, w: &mut Walk, payload: u64, plen: u64) {
    let Some(t) = w.track else { return };
    let Some(d) = src.read_vec(payload, plen.min(96), 96) else {
        return;
    };
    let Some((v, c, m, at)) = version_times(&d) else {
        return;
    };
    let n = info.streams[t].index;
    push_time(info, format!("tkhd[{}].creation", n), c);
    push_time(info, format!("tkhd[{}].modification", n), m);
    // track_id (4), reserved (4), duration (4 or 8)
    let dur = if v == 1 {
        be64(&d, at + 8)
    } else {
        be32(&d, at + 8).map(|x| x as u64)
    };
    if let (Some(dur), Some(ts)) = (dur, w.b.movie_timescale)
        && let Some(x) = ms(dur, ts)
        && dur != u32::MAX as u64
        && dur != u64::MAX
    {
        info.duration(&format!("tkhd[{}]", n), x, "declared");
    }
    let wh = d.len().checked_sub(8);
    if let Some(p) = wh
        && d.len() >= if v == 1 { 96 } else { 84 }
    {
        let wd = be32(&d, p).unwrap_or(0) >> 16;
        let ht = be32(&d, p + 4).unwrap_or(0) >> 16;
        if wd > 0 && ht > 0 {
            info.streams[t].width.get_or_insert(wd);
            info.streams[t].height.get_or_insert(ht);
        }
    }
}

fn mdhd(src: &mut dyn Source, info: &mut MediaInfo, w: &mut Walk, payload: u64, plen: u64) {
    let Some(t) = w.track else { return };
    let Some(d) = src.read_vec(payload, plen.min(36), 36) else {
        return;
    };
    let Some((v, c, m, at)) = version_times(&d) else {
        return;
    };
    let n = info.streams[t].index;
    push_time(info, format!("mdhd[{}].creation", n), c);
    push_time(info, format!("mdhd[{}].modification", n), m);
    let ts = be32(&d, at).unwrap_or(0);
    let (dur, lang_at) = if v == 1 {
        (be64(&d, at + 4), at + 12)
    } else {
        (be32(&d, at + 4).map(|x| x as u64), at + 8)
    };
    let s = &mut info.streams[t];
    s.timescale = Some(ts);
    if let Some(l) = be16(&d, lang_at)
        && l != 0
        && l != 0x7FFF
    {
        let c = |sh: u16| (((l >> sh) & 0x1F) as u8 + 0x60) as char;
        let lang: String = [c(10), c(5), c(0)].iter().collect();
        if lang.chars().all(|c| c.is_ascii_lowercase()) {
            s.language = Some(lang);
        }
    }
    if let Some(dur) = dur
        && dur != u32::MAX as u64
        && dur != u64::MAX
        && let Some(x) = ms(dur, ts)
    {
        s.duration_ms = Some(x);
        info.duration(&format!("mdhd[{}]", n), x, "declared");
    }
}

fn hdlr(
    src: &mut dyn Source,
    info: &mut MediaInfo,
    w: &mut Walk,
    payload: u64,
    plen: u64,
    parent: &[u8; 4],
) {
    let Some(d) = src.read_vec(payload, plen.min(1024), 1024) else {
        return;
    };
    if d.len() < 12 {
        return;
    }
    let handler = fourcc(&d[8..12]);
    let mut name = d.get(24..).map(|n| n.to_vec()).unwrap_or_default();
    // QuickTime stores a Pascal string (length byte first).
    if let Some(&l) = name.first()
        && l as usize == name.len() - 1
        && l > 0
    {
        name.remove(0);
    }
    let name = crate::util::clean_text(&text(&name));
    if parent == b"mdia"
        && let Some(t) = w.track
    {
        let s = &mut info.streams[t];
        s.kind = match handler.as_str() {
            "vide" => "video",
            "soun" => "audio",
            "text" | "sbtl" | "subt" | "clcp" => "text",
            "meta" | "mdta" | "tmcd" => "metadata",
            _ => "other",
        }
        .into();
        if !name.is_empty() {
            s.handler_name = Some(name.clone());
            let idx = s.index;
            info.encoder(&format!("hdlr[{}] name", idx), &name);
        }
    }
}

fn stsd(src: &mut dyn Source, info: &mut MediaInfo, w: &mut Walk, payload: u64, plen: u64) {
    let Some(t) = w.track else { return };
    let Some(d) = src.read_vec(payload, plen.min(256), 256) else {
        return;
    };
    let count = be32(&d, 4).unwrap_or(0);
    let e = 8usize; // first sample entry
    let Some(fmt) = d.get(e + 4..e + 8) else {
        return;
    };
    let kind = info.streams[t].kind.clone();
    let s = &mut info.streams[t];
    s.codec = fourcc(fmt);
    let mut vendor = None;
    if kind == "audio" {
        s.channels = be16(&d, e + 24);
        s.bits_per_sample = be16(&d, e + 26);
        s.sample_rate = be32(&d, e + 32).map(|r| r >> 16).filter(|r| *r > 0);
        vendor = d.get(e + 20..e + 24).map(|v| v.to_vec());
    } else if kind == "video" {
        if let (Some(wd), Some(ht)) = (be16(&d, e + 32), be16(&d, e + 34))
            && wd > 0
            && ht > 0
        {
            s.width = Some(wd as u32);
            s.height = Some(ht as u32);
        }
        vendor = d.get(e + 20..e + 24).map(|v| v.to_vec());
        if let Some(&l) = d.get(e + 50) {
            let l = (l as usize).min(31);
            if let Some(n) = d.get(e + 51..e + 51 + l) {
                let n = crate::util::clean_text(&text(n));
                if !n.is_empty() {
                    s.compressor_name = Some(n.clone());
                    let idx = s.index;
                    info.encoder(&format!("stsd[{}] compressor", idx), &n);
                }
            }
        }
    }
    if let Some(v) = vendor
        && v.iter().any(|&c| c != 0)
    {
        let idx = info.streams[t].index;
        info.encoder(&format!("stsd[{}] vendor", idx), &fourcc(&v));
    }
    if count > 1 {
        info.note(format!(
            "track {} has {} sample descriptions; the first is reported",
            info.streams[t].index, count
        ));
    }
}

fn stts(src: &mut dyn Source, info: &mut MediaInfo, w: &mut Walk, payload: u64, plen: u64) {
    let Some(t) = w.track else { return };
    let Some(h) = src.read_vec(payload, 8.min(plen), 8) else {
        return;
    };
    let count = be32(&h, 4).unwrap_or(0) as u64;
    let fits = plen.saturating_sub(8) / 8;
    if count > fits {
        info.anomaly(
            AnomalyKind::SizeInconsistent,
            payload,
            format!(
                "stts of track {} declares {} entries, the box holds {}",
                info.streams[t].index, count, fits
            ),
        );
    }
    let n = count.min(fits).min(MAX_TABLE_ENTRIES);
    if n < count.min(fits) {
        info.note("stts longer than the parser cap: computed duration not available");
        return;
    }
    let (mut samples, mut total) = (0u128, 0u128);
    let mut at = payload + 8;
    let mut left = n;
    while left > 0 {
        let k = left.min(8192);
        let Some(d) = src.read_vec(at, k * 8, MAX_STRUCT_BYTES) else {
            return;
        };
        for e in d.chunks_exact(8) {
            let c = be32(e, 0).unwrap_or(0) as u128;
            let delta = be32(e, 4).unwrap_or(0) as u128;
            samples += c;
            total += c * delta;
        }
        at += k * 8;
        left -= k;
    }
    let s = &mut info.streams[t];
    s.samples = Some(samples.min(u64::MAX as u128) as u64);
    let idx = s.index;
    if let Some(ts) = s.timescale
        && ts > 0
        && samples > 0
    {
        let x = (total * 1000 / ts as u128).min(u64::MAX as u128) as u64;
        info.duration(&format!("stts[{}]", idx), x, "computed");
    }
}

/// QuickTime `udta` text atom (©xxx): one or more [u16 length][u16 language][text], or an iTunes-style
/// `data` atom.
fn udta_text(src: &mut dyn Source, info: &mut MediaInfo, typ: &[u8; 4], payload: u64, plen: u64) {
    let d = src.read_upto(payload, plen, 64 * 1024);
    let key = fourcc(typ);
    if d.get(4..8) == Some(b"data") {
        if let Some(v) = data_atom_value(&d) {
            udta_item(info, "udta", &key, &v);
        }
        return;
    }
    if let Some(l) = be16(&d, 0)
        && let Some(v) = d.get(4..4 + l as usize)
    {
        udta_item(info, "udta", &key, &text(v));
    }
}

fn udta_item(info: &mut MediaInfo, source: &str, key: &str, value: &str) {
    info.tag(source, key, value);
    let k = key.to_ascii_lowercase();
    match k.as_str() {
        "©too" | "©enc" | "©swr" | "©swf" | "©fmt" | "encoder" | "©inf" => {
            info.encoder(&format!("{} {}", source, key), value)
        }
        "©mak" | "©mod" | "make" | "model" => {
            let v = crate::util::clean_text(value);
            if !v.is_empty() {
                info.device_claims.push(crate::model::Tag {
                    source: source.into(),
                    key: key.into(),
                    value: v,
                });
            }
        }
        "©day" | "date" => tag_time(info, &format!("{} {}", source, key), value),
        "©xyz" => {}
        _ => {}
    }
}

fn tag_time(info: &mut MediaInfo, source: &str, value: &str) {
    let parsed = crate::util::parse_tag_date(value);
    info.time(TimeValue {
        source: source.into(),
        raw: crate::util::clean_text(value),
        unix: parsed.map(|p| p.0),
        utc: parsed.and_then(|p| rfc3339(p.0)),
        origin: if parsed.is_some_and(|p| p.1) {
            "tag".into()
        } else {
            "tag_local".into()
        },
    });
}

/// Value of a `data` atom (bytes from its start): text types decoded, integers as decimal.
fn data_atom_value(d: &[u8]) -> Option<String> {
    let size = be32(d, 0)? as usize;
    if d.get(4..8)? != b"data" || size < 16 {
        return None;
    }
    let ty = be32(d, 8)? & 0x00FF_FFFF;
    let v = d.get(16..size.min(d.len()))?;
    match ty {
        1 | 4 | 5 => Some(text(v)),
        2 | 6 => Some(String::from_utf16_lossy(
            &v.chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        )),
        21 | 22 if v.len() <= 8 && !v.is_empty() => {
            let mut x: u64 = 0;
            for &b in v {
                x = (x << 8) | b as u64;
            }
            Some(x.to_string())
        }
        13 | 14 | 27 => Some(format!("[image, {} bytes]", v.len())),
        0 if v.iter().all(|c| (0x20..0x7f).contains(c)) => Some(text(v)),
        _ => Some(format!("[binary, {} bytes]", v.len())),
    }
}

fn keys(src: &mut dyn Source, info: &mut MediaInfo, w: &mut Walk, payload: u64, plen: u64) {
    let d = src.read_upto(payload, plen, MAX_STRUCT_BYTES);
    let count = be32(&d, 4).unwrap_or(0);
    let mut at = 8usize;
    w.keys.clear();
    for _ in 0..count.min(4096) {
        let Some(sz) = be32(&d, at) else { break };
        let sz = sz as usize;
        if sz < 8 || at + sz > d.len() {
            info.anomaly(
                AnomalyKind::SizeInconsistent,
                payload + at as u64,
                "keys entry size does not fit the keys box",
            );
            break;
        }
        let name = text(&d[at + 8..at + sz]);
        if name.starts_with("com.apple.quicktime")
            && !w
                .b
                .vendor_traces
                .iter()
                .any(|t| t == "apple_quicktime_keys")
        {
            w.b.vendor_traces.push("apple_quicktime_keys".into());
        }
        if name.starts_with("com.android") && !w.b.vendor_traces.iter().any(|t| t == "android_keys")
        {
            w.b.vendor_traces.push("android_keys".into());
        }
        w.keys.push(name);
        at += sz;
    }
}

fn ilst(src: &mut dyn Source, info: &mut MediaInfo, w: &mut Walk, payload: u64, plen: u64) {
    let d = src.read_upto(payload, plen, MAX_STRUCT_BYTES);
    if (d.len() as u64) < plen {
        info.note("ilst larger than the parser cap: only its start is read");
    }
    let mut at = 0usize;
    let mut items = 0;
    while at + 8 <= d.len() && items < 4096 {
        items += 1;
        let sz = be32(&d, at).unwrap_or(0) as usize;
        if sz < 8 || at + sz > d.len() {
            info.anomaly(
                AnomalyKind::SizeInconsistent,
                payload + at as u64,
                "ilst item size does not fit the ilst box",
            );
            break;
        }
        let item = &d[at..at + sz];
        let raw_key = &item[4..8];
        let idx = u32::from_be_bytes([raw_key[0], raw_key[1], raw_key[2], raw_key[3]]) as usize;
        // QuickTime metadata: the item type is a 1-based index into `keys`.
        let key = if !w.keys.is_empty() && idx >= 1 && idx <= w.keys.len() {
            w.keys[idx - 1].clone()
        } else {
            fourcc(raw_key)
        };
        let mut name_suffix = String::new();
        let mut j = 8usize;
        let mut value = None;
        while j + 8 <= item.len() {
            let s = be32(item, j).unwrap_or(0) as usize;
            if s < 8 || j + s > item.len() {
                break;
            }
            let t = &item[j + 4..j + 8];
            if t == b"name" && s > 12 {
                name_suffix = text(&item[j + 12..j + s]);
            } else if t == b"data" {
                value = data_atom_value(&item[j..j + s]);
            }
            j += s;
        }
        let key = if key == "----" && !name_suffix.is_empty() {
            format!("----:{}", name_suffix)
        } else {
            key
        };
        if let Some(v) = value {
            meta_item(info, &key, &v);
        }
        at += sz;
    }
}

fn meta_item(info: &mut MediaInfo, key: &str, value: &str) {
    let k = key.to_ascii_lowercase();
    if k.starts_with("com.") {
        info.tag("keys", key, value);
        if k.ends_with(".make") || k.ends_with(".model") || k.ends_with(".manufacturer") {
            let v = crate::util::clean_text(value);
            if !v.is_empty() {
                info.device_claims.push(crate::model::Tag {
                    source: "keys".into(),
                    key: key.into(),
                    value: v,
                });
            }
        }
        if k.ends_with(".software") || k == "com.android.version" {
            info.encoder(&format!("keys {}", key), value);
        }
        if k.ends_with(".creationdate") {
            tag_time(info, &format!("keys {}", key), value);
        }
        return;
    }
    udta_item(info, "ilst", key, value);
}

/// Notes after the walk (multiple mdat and moov position are judged in `signals.rs`).
fn finish(info: &mut MediaInfo, w: &mut Walk) {
    if w.b.moov_count == 0 && !w.b.top_level.is_empty() {
        info.note("no moov box: no track or time information");
    }
}
