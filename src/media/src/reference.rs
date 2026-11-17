//! Reference comparison: a file known to come from the claimed device or app (`--reference`) gives a
//! structural fingerprint; the evidence's fingerprint is compared aspect by aspect. Differences are
//! signals for the examiner (a device update, another recording mode or app setting also changes them).

use crate::model::{MediaInfo, Signal};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Aspects compared, in report order.
pub const ASPECTS: [&str; 11] = [
    "container",
    "brands",
    "top_level_layout",
    "codecs",
    "audio_parameters",
    "video_size",
    "handler_names",
    "encoders",
    "tag_keys",
    "timescales",
    "chunk_or_frame_layout",
];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// aspect → value (strings so the table is easy to read and to diff).
    pub aspects: BTreeMap<String, String>,
}

fn join<I: IntoIterator<Item = String>>(it: I) -> String {
    let v: Vec<String> = it.into_iter().filter(|s| !s.is_empty()).collect();
    if v.is_empty() {
        "—".into()
    } else {
        v.join(", ")
    }
}

/// Strips digits so "Lavf60.16.100" and "Lavf60.3.100" compare equal on the software, and an OS update
/// ("17.1" → "17.2") does not count as a different writer; the exact strings stay in the table.
fn family(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_ascii_digit() && *c != '.')
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn fingerprint(info: &MediaInfo) -> Fingerprint {
    let mut a = BTreeMap::new();
    a.insert("container".into(), info.container.key().to_string());
    a.insert(
        "brands".into(),
        info.brand
            .as_ref()
            .map(|b| format!("{} [{}]", b.major, b.compatible.join(" ")))
            .unwrap_or_else(|| "—".into()),
    );
    a.insert(
        "top_level_layout".into(),
        info.bmff
            .as_ref()
            .map(|b| {
                // Consecutive duplicates collapsed: two free boxes in a row are one layout step.
                let mut v: Vec<String> = vec![];
                for t in &b.top_level {
                    if v.last() != Some(t) {
                        v.push(t.clone());
                    }
                }
                v.join(" > ")
            })
            .or_else(|| info.wav.as_ref().map(|w| w.chunks.join(" > ")))
            .unwrap_or_else(|| "—".into()),
    );
    a.insert(
        "codecs".into(),
        join(
            info.streams
                .iter()
                .map(|s| format!("{}:{}", s.kind, s.codec)),
        ),
    );
    a.insert(
        "audio_parameters".into(),
        join(info.streams.iter().filter(|s| s.kind == "audio").map(|s| {
            format!(
                "{} Hz {} ch {} bit",
                s.sample_rate.map(|x| x.to_string()).unwrap_or("?".into()),
                s.channels.map(|x| x.to_string()).unwrap_or("?".into()),
                s.bits_per_sample
                    .map(|x| x.to_string())
                    .unwrap_or("?".into())
            )
        })),
    );
    a.insert(
        "video_size".into(),
        join(
            info.streams
                .iter()
                .filter(|s| s.kind == "video")
                .map(|s| format!("{}x{}", s.width.unwrap_or(0), s.height.unwrap_or(0))),
        ),
    );
    a.insert(
        "handler_names".into(),
        join(
            info.streams
                .iter()
                .map(|s| s.handler_name.clone().unwrap_or_default()),
        ),
    );
    let mut enc: Vec<String> = info
        .encoders
        .iter()
        .filter(|e| !e.source.starts_with("hdlr"))
        .map(|e| format!("{}: {}", e.source, family(&e.value)))
        .collect();
    enc.sort();
    enc.dedup();
    a.insert("encoders".into(), join(enc));
    let mut keys: Vec<String> = info
        .tags
        .iter()
        .map(|t| format!("{}:{}", t.source, t.key))
        .collect();
    keys.sort();
    keys.dedup();
    a.insert("tag_keys".into(), join(keys));
    a.insert(
        "timescales".into(),
        join(
            info.bmff
                .as_ref()
                .and_then(|b| b.movie_timescale)
                .map(|t| format!("movie {}", t))
                .into_iter()
                .chain(
                    info.streams
                        .iter()
                        .filter_map(|s| s.timescale.map(|t| format!("{} {}", s.kind, t))),
                ),
        ),
    );
    a.insert(
        "chunk_or_frame_layout".into(),
        if let Some(m) = &info.mp3 {
            format!(
                "{} {}, Xing/Info: {}, ID3v2 {}, ID3v1 {}",
                m.mpeg_version.clone().unwrap_or_default(),
                if m.vbr { "VBR" } else { "CBR" },
                m.xing
                    .as_ref()
                    .map(|x| x.tag.clone())
                    .unwrap_or("none".into()),
                m.id3v2_version.clone().unwrap_or("none".into()),
                if m.id3v1 { "yes" } else { "no" }
            )
        } else if let Some(o) = &info.ogg {
            join(o.streams.iter().map(|s| {
                format!(
                    "{} vendor {}",
                    s.codec,
                    s.vendor.clone().map(|v| family(&v)).unwrap_or_default()
                )
            }))
        } else if let Some(b) = &info.bmff {
            format!(
                "mdat {}, edit lists {}, free boxes {}, fragmented {}",
                b.mdat.len(),
                b.edit_lists,
                b.free_boxes,
                b.fragmented
            )
        } else {
            "—".into()
        },
    );
    Fingerprint { aspects: a }
}

/// One row of the comparison table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub aspect: String,
    pub evidence: String,
    pub reference: String,
    pub same: bool,
}

pub fn compare(evidence: &Fingerprint, reference: &Fingerprint) -> Vec<Row> {
    ASPECTS
        .iter()
        .map(|k| {
            let e = evidence.aspects.get(*k).cloned().unwrap_or_default();
            let r = reference.aspects.get(*k).cloned().unwrap_or_default();
            Row {
                aspect: k.to_string(),
                same: e == r,
                evidence: e,
                reference: r,
            }
        })
        .collect()
}

/// REFERENCE_CONSISTENT (info) or REFERENCE_DIFFERS (medium) for one reference file.
pub fn signal(reference_name: &str, rows: &[Row]) -> Signal {
    let diff: Vec<String> = rows
        .iter()
        .filter(|r| !r.same)
        .map(|r| r.aspect.clone())
        .collect();
    if diff.is_empty() {
        Signal::new("REFERENCE_CONSISTENT", "info")
            .with("reference", reference_name)
            .with("aspects", rows.len().to_string())
    } else {
        Signal::new("REFERENCE_DIFFERS", "medium")
            .with("reference", reference_name)
            .with("differences", diff.join(", "))
            .with("count", diff.len().to_string())
    }
}
