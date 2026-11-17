//! Turns a parsed [`MediaInfo`] into signals. Every code is worded in the i18n catalogues (title, what it
//! shows, what it does not prove). Severities are conservative: no media signal is `high` (a high finding
//! is reserved for a failed analysis, evidence that changed or differs from the case registry).
//!
//! Thresholds (also in docs/theory.md):
//! - durations differ when they differ by more than 5 % **and** more than 200 ms;
//! - header times of one file disagree when a track's creation time differs from the movie's by more
//!   than 60 s, or a modification time precedes its creation time by more than 2 s;
//! - a tag date differs from the header creation time when they are more than 26 h apart (tags are often
//!   local time without a zone);
//! - a file-system modification time "before" the media time needs a 26 h margin too.

use crate::model::{AnomalyKind, Container, EncoderCategory, MediaInfo, Signal};
use std::collections::BTreeSet;

/// What the caller knows besides the bytes.
#[derive(Clone, Debug, Default)]
pub struct SignalCtx {
    /// Lower-case extension of the file name, if any.
    pub extension: Option<String>,
    /// File-system modification time (Unix seconds) of the evidence, if readable.
    pub fs_modified: Option<i64>,
    /// Analysis time (Unix seconds).
    pub now: i64,
}

pub const DURATION_REL_PCT: u64 = 5;
pub const DURATION_ABS_MS: u64 = 200;
pub const TRACK_TIME_TOLERANCE_S: i64 = 60;
pub const TAG_TIME_TOLERANCE_S: i64 = 26 * 3600;

fn anomaly_signal(info: &MediaInfo, kind: AnomalyKind, code: &str, sev: &str) -> Option<Signal> {
    let n = info.anomaly_count(kind);
    let first = info.first_anomaly(kind)?;
    Some(
        Signal::new(code, sev)
            .with("count", n.to_string())
            .with("offset", first.offset.to_string())
            .with("detail", first.detail.clone()),
    )
}

fn durations_differ(a: u64, b: u64) -> bool {
    let d = a.abs_diff(b);
    d > DURATION_ABS_MS && d * 100 > a.max(b) * DURATION_REL_PCT
}

pub fn signals(info: &MediaInfo, ctx: &SignalCtx) -> Vec<Signal> {
    let mut out = vec![];
    let ext = ctx.extension.as_deref().unwrap_or("");
    let claimed = Container::from_extension(ext);

    // Container identity.
    if info.container == Container::Unknown {
        if claimed.is_some() {
            out.push(Signal::new("MEDIA_NOT_RECOGNISED", "low").with("extension", ext));
        }
        return out;
    }
    if let Some(c) = claimed
        && c != info.container
    {
        out.push(
            Signal::new("CONTAINER_EXTENSION_MISMATCH", "medium")
                .with("container", info.container.name())
                .with("extension", ext),
        );
    }
    if let Some(b) = &info.brand {
        let brands: Vec<&str> = std::iter::once(b.major.as_str())
            .chain(b.compatible.iter().map(|s| s.as_str()))
            .collect();
        let has = |p: &str| brands.iter().any(|x| x.starts_with(p));
        let odd = match ext {
            "mov" | "qt" => !has("qt") && (b.major.starts_with("3g") || b.major.starts_with("M4")),
            "mp4" | "m4a" | "m4v" | "m4b" | "3gp" | "3g2" if b.major == "qt  " => true,
            "3gp" | "3g2" => !has("3g"),
            _ => false,
        };
        if odd {
            out.push(
                Signal::new("BRAND_EXTENSION_MISMATCH", "low")
                    .with("brand", b.major.clone())
                    .with("compatible", b.compatible.join(", "))
                    .with("extension", ext),
            );
        }
    }

    // Structure.
    for (kind, code, sev) in [
        (
            AnomalyKind::SizeInconsistent,
            "STRUCTURE_SIZE_INCONSISTENT",
            "medium",
        ),
        (AnomalyKind::Truncated, "TRUNCATED_STREAM", "medium"),
        (AnomalyKind::SyncGap, "FRAME_SYNC_GAPS", "medium"),
        (AnomalyKind::CrcMismatch, "OGG_CRC_MISMATCH", "medium"),
        (AnomalyKind::SequenceGap, "OGG_PAGE_SEQUENCE_GAP", "medium"),
        (
            AnomalyKind::GranuleNotMonotonic,
            "OGG_GRANULE_NOT_MONOTONIC",
            "medium",
        ),
        (AnomalyKind::ChainedStreams, "OGG_CHAINED_STREAMS", "low"),
        (
            AnomalyKind::ParametersChange,
            "STREAM_PARAMETERS_CHANGE",
            "medium",
        ),
        (
            AnomalyKind::FormatParameters,
            "FORMAT_PARAMETERS_INCONSISTENT",
            "medium",
        ),
        (AnomalyKind::LimitReached, "PARSER_LIMIT_REACHED", "info"),
    ] {
        if let Some(s) = anomaly_signal(info, kind, code, sev) {
            out.push(s);
        }
    }
    if let Some(a) = info.first_anomaly(AnomalyKind::TrailingData) {
        let bytes = info.file_size.saturating_sub(info.logical_end);
        let zero = a.detail.contains("all zero");
        out.push(
            Signal::new("TRAILING_DATA", if zero { "low" } else { "medium" })
                .with("bytes", bytes.to_string())
                .with("offset", a.offset.to_string())
                .with("detail", a.detail.clone()),
        );
    }
    if let Some(b) = &info.bmff {
        if b.mdat.len() > 1 && !b.fragmented {
            out.push(Signal::new("MULTIPLE_MDAT", "low").with("count", b.mdat.len().to_string()));
        }
        if let (Some(moov), Some((mdat, _))) = (b.moov_offset, b.mdat.first())
            && moov > *mdat
            && !b.fragmented
        {
            out.push(
                Signal::new("MOOV_AFTER_MDAT", "info")
                    .with("layout", b.top_level.join(" > "))
                    .with(
                        "brand",
                        info.brand
                            .as_ref()
                            .map(|b| b.major.clone())
                            .unwrap_or_default(),
                    ),
            );
        }
        if !b.vendor_traces.is_empty() {
            out.push(
                Signal::new("VENDOR_TRACES", "info").with("traces", capped(&b.vendor_traces, 8)),
            );
        }
    }

    // Encoders and claimed device.
    encoder_signals(info, &mut out);

    // Times.
    time_signals(info, ctx, &mut out);

    // Durations.
    duration_signals(info, &mut out);

    if info.tags.is_empty() && info.encoders.is_empty() && info.device_claims.is_empty() {
        out.push(Signal::new("METADATA_ABSENT", "info").with("container", info.container.name()));
    }
    out
}

fn capped(v: &[String], n: usize) -> String {
    let mut s = v.iter().take(n).cloned().collect::<Vec<_>>().join(", ");
    if v.len() > n {
        s.push_str(&format!(" … (+{})", v.len() - n));
    }
    s
}

fn encoder_signals(info: &MediaInfo, out: &mut Vec<Signal>) {
    let mut seen = BTreeSet::new();
    let mut listed = vec![];
    let mut editing_label = None;
    for e in &info.encoders {
        listed.push(if e.label.is_empty() {
            format!("{} ({})", e.value, e.source)
        } else {
            format!("{} = {} ({})", e.value, e.label, e.source)
        });
        let (code, sev) = match e.category {
            EncoderCategory::Editor => ("EDITING_SOFTWARE_TRACE", "medium"),
            EncoderCategory::OnlineConverter => ("ONLINE_CONVERTER_TRACE", "medium"),
            EncoderCategory::Transcoder => ("TRANSCODER_TRACE", "low"),
            EncoderCategory::Messenger => ("MESSENGER_TRACE", "low"),
            _ => continue,
        };
        if editing_label.is_none() {
            editing_label = Some(e.label.clone());
        }
        if seen.insert((code, e.label.clone())) {
            out.push(
                Signal::new(code, sev)
                    .with("software", e.label.clone())
                    .with("value", e.value.clone())
                    .with("source", e.source.clone()),
            );
        }
    }
    if !listed.is_empty() {
        out.push(Signal::new("ENCODER_IDENTIFIED", "info").with("encoders", capped(&listed, 6)));
    }
    let device_software = info.encoders.iter().any(|e| {
        matches!(
            e.category,
            EncoderCategory::Device | EncoderCategory::RecorderApp
        )
    });
    if !info.device_claims.is_empty() || device_software {
        let claim = if info.device_claims.is_empty() {
            info.encoders
                .iter()
                .find(|e| {
                    matches!(
                        e.category,
                        EncoderCategory::Device | EncoderCategory::RecorderApp
                    )
                })
                .map(|e| format!("{} ({})", e.value, e.source))
                .unwrap_or_default()
        } else {
            capped(
                &info
                    .device_claims
                    .iter()
                    .map(|t| format!("{} = {}", t.key, t.value))
                    .collect::<Vec<_>>(),
                4,
            )
        };
        out.push(Signal::new("DEVICE_CLAIM", "info").with("device", claim.clone()));
        if let Some(l) = editing_label {
            out.push(
                Signal::new("ENCODER_CONFLICTS_WITH_DEVICE_CLAIM", "medium")
                    .with("device", claim)
                    .with("software", l),
            );
        }
    }
}

fn utc(t: i64) -> String {
    crate::util::rfc3339(t).unwrap_or_else(|| t.to_string())
}

fn time_signals(info: &MediaInfo, ctx: &SignalCtx, out: &mut Vec<Signal>) {
    let header: Vec<_> = info.times.iter().filter(|t| t.origin == "header").collect();
    let get = |src: &str| header.iter().find(|t| t.source == src).and_then(|t| t.unix);
    let movie_c = get("mvhd.creation");
    let movie_m = get("mvhd.modification");
    if !header.is_empty() && header.iter().all(|t| t.unix.is_none()) {
        out.push(Signal::new("MEDIA_TIMES_ZERO", "info").with("count", header.len().to_string()));
    }
    let mut problems = vec![];
    for t in &header {
        if let Some(base) = t.source.strip_suffix(".creation")
            && let (Some(c), Some(m)) = (t.unix, get(&format!("{}.modification", base)))
            && m + 2 < c
        {
            problems.push(format!(
                "{}: modification {} before creation {}",
                base,
                utc(m),
                utc(c)
            ));
        }
        if t.source != "mvhd.creation"
            && t.source.ends_with(".creation")
            && let (Some(c), Some(mc)) = (t.unix, movie_c)
            && (c - mc).abs() > TRACK_TIME_TOLERANCE_S
        {
            problems.push(format!(
                "{} {} vs mvhd.creation {}",
                t.source,
                utc(c),
                utc(mc)
            ));
        }
    }
    if !problems.is_empty() {
        out.push(
            Signal::new("MEDIA_TIMES_INCONSISTENT", "medium")
                .with("count", problems.len().to_string())
                .with("detail", capped(&problems, 3)),
        );
    }
    if let (Some(c), Some(m)) = (movie_c, movie_m)
        && m > c + TRACK_TIME_TOLERANCE_S
    {
        out.push(
            Signal::new("MEDIA_MODIFIED_AFTER_CREATION", "low")
                .with("created", utc(c))
                .with("modified", utc(m)),
        );
    }
    // Header creation (movie, else the first header time set) vs dates in tags.
    let header_c = movie_c.or_else(|| header.iter().find_map(|t| t.unix));
    if let Some(hc) = header_c {
        for t in info.times.iter().filter(|t| t.origin != "header") {
            if let Some(tv) = t.unix
                && (tv - hc).abs() > TAG_TIME_TOLERANCE_S
            {
                out.push(
                    Signal::new("TAG_TIME_DIFFERS_FROM_HEADER", "low")
                        .with("tag_source", t.source.clone())
                        .with("tag_time", t.raw.clone())
                        .with("header_time", utc(hc)),
                );
                break;
            }
        }
    }
    if ctx.now > 0
        && let Some(t) = info
            .times
            .iter()
            .find(|t| t.unix.is_some_and(|u| u > ctx.now + 86_400))
    {
        out.push(
            Signal::new("MEDIA_TIME_IN_FUTURE", "low")
                .with("source", t.source.clone())
                .with("time", t.utc.clone().unwrap_or_else(|| t.raw.clone())),
        );
    }
    let media_time = header_c.or_else(|| {
        info.times
            .iter()
            .filter(|t| t.origin == "tag")
            .find_map(|t| t.unix)
    });
    if let (Some(fs), Some(mt)) = (ctx.fs_modified, media_time)
        && fs + TAG_TIME_TOLERANCE_S < mt
    {
        out.push(
            Signal::new("FILE_TIME_BEFORE_MEDIA_TIME", "low")
                .with("file_modified", utc(fs))
                .with("media_time", utc(mt)),
        );
    }
}

fn duration_signals(info: &MediaInfo, out: &mut Vec<Signal>) {
    let d = |s: &str| info.durations.iter().find(|x| x.source == s).map(|x| x.ms);
    let mut pairs: Vec<(String, u64, String, u64)> = vec![];
    // ISO BMFF: per track, mdhd (declared) vs the sample table (computed); mvhd vs the longest track.
    if let Some(b) = &info.bmff
        && !b.fragmented
    {
        let mut longest = None::<(String, u64)>;
        for s in &info.streams {
            let (m, st) = (
                d(&format!("mdhd[{}]", s.index)),
                d(&format!("stts[{}]", s.index)),
            );
            if let (Some(m), Some(st)) = (m, st)
                && m > 0
            {
                pairs.push((
                    format!("mdhd[{}]", s.index),
                    m,
                    format!("stts[{}]", s.index),
                    st,
                ));
            }
            if let Some(t) = d(&format!("tkhd[{}]", s.index))
                && longest.as_ref().is_none_or(|l| t > l.1)
            {
                longest = Some((format!("tkhd[{}]", s.index), t));
            }
        }
        if let (Some(mv), Some((src, t))) = (d("mvhd"), longest)
            && mv > 0
            && b.edit_lists == 0
        {
            pairs.push(("mvhd".into(), mv, src, t));
        }
    }
    // WAVE: data size field vs bytes present; fact vs data.
    if let (Some(a), Some(b)) = (d("data chunk size field"), d("data bytes present")) {
        pairs.push((
            "data chunk size field".into(),
            a,
            "data bytes present".into(),
            b,
        ));
    }
    // MP3: Xing/Info/VBRI frame count vs frames counted; ID3 TLEN vs frames counted.
    if let Some(c) = d("frames counted") {
        for x in &info.durations {
            if x.kind == "declared"
                && (x.source.ends_with("frame count") || x.source.ends_with("TLEN"))
            {
                pairs.push((x.source.clone(), x.ms, "frames counted".into(), c));
            }
        }
    }
    for (a, am, b, bm) in pairs {
        if durations_differ(am, bm) {
            out.push(
                Signal::new("DURATION_MISMATCH", "medium")
                    .with("declared_source", a)
                    .with("declared_ms", am.to_string())
                    .with("computed_source", b)
                    .with("computed_ms", bm.to_string()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_tolerance() {
        assert!(!durations_differ(1000, 1100), "100 ms is within 200 ms");
        assert!(!durations_differ(100_000, 103_000), "3 % is within 5 %");
        assert!(durations_differ(1000, 2000));
        assert!(durations_differ(100_000, 110_000));
    }

    #[test]
    fn unknown_content_under_a_media_name() {
        let info = MediaInfo::new(Container::Unknown, 10);
        let s = signals(
            &info,
            &SignalCtx {
                extension: Some("mp4".into()),
                ..SignalCtx::default()
            },
        );
        assert_eq!(s[0].code, "MEDIA_NOT_RECOGNISED");
        assert!(signals(&info, &SignalCtx::default()).is_empty());
    }
}
