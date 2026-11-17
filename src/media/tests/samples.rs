//! The synthetic samples (tests/make_samples.py) produce exactly the media signals listed in
//! examples/sample/expected_findings.json (codes added by the core pipeline — EMPTY_FILE, duplicates — are
//! left out here; the CLI end-to-end test checks the full list).

use rv_avauth_media::signals::{SignalCtx, signals};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const CORE_CODES: &[&str] = &["EMPTY_FILE", "DUPLICATE_CONTENT", "EXTENSION_MISMATCH"];

fn sample_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sample")
}

#[test]
fn samples_produce_the_expected_signals() {
    let expected: BTreeMap<String, Vec<String>> = serde_json::from_str(
        &std::fs::read_to_string(sample_dir().join("expected_findings.json")).unwrap(),
    )
    .unwrap();
    let mut failures = vec![];
    for (name, codes) in &expected {
        let bytes = std::fs::read(sample_dir().join("evidence").join(name)).unwrap();
        let info = rv_avauth_media::parse_bytes(&bytes);
        let ext = Path::new(name)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        let got: BTreeSet<String> = signals(
            &info,
            &SignalCtx {
                extension: ext,
                fs_modified: None,
                now: 1_800_000_000,
            },
        )
        .into_iter()
        .map(|s| s.code)
        .collect();
        let want: BTreeSet<String> = codes
            .iter()
            .filter(|c| !CORE_CODES.contains(&c.as_str()))
            .cloned()
            .collect();
        if got != want {
            failures.push(format!(
                "{name}: got {:?}, expected {:?}\n  anomalies {:?}\n  durations {:?}\n  encoders {:?}",
                got, want, info.anomalies, info.durations, info.encoders
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn reference_comparison_of_the_samples() {
    use rv_avauth_media::reference::{compare, fingerprint, signal};
    let read =
        |p: &str| rv_avauth_media::parse_bytes(&std::fs::read(sample_dir().join(p)).unwrap());
    let reference = fingerprint(&read("reference/camera-reference.mov"));
    let same = compare(
        &fingerprint(&read("evidence/camera-original.mov")),
        &reference,
    );
    assert_eq!(
        signal("camera-reference.mov", &same).code,
        "REFERENCE_CONSISTENT"
    );
    let other = compare(
        &fingerprint(&read("evidence/voice-reencoded.m4a")),
        &reference,
    );
    let s = signal("camera-reference.mov", &other);
    assert_eq!(s.code, "REFERENCE_DIFFERS");
    for aspect in ["brands", "top_level_layout", "handler_names", "encoders"] {
        assert!(
            s.params["differences"].contains(aspect),
            "{aspect}: {:?}",
            s.params
        );
    }
}

#[test]
fn parsed_details_of_the_samples() {
    let read = |p: &str| {
        rv_avauth_media::parse_bytes(&std::fs::read(sample_dir().join("evidence").join(p)).unwrap())
    };
    let mov = read("camera-original.mov");
    assert_eq!(mov.brand.as_ref().unwrap().major, "qt  ");
    assert_eq!(mov.streams[0].codec, "sowt");
    assert_eq!(mov.streams[0].sample_rate, Some(8000));
    assert_eq!(
        mov.streams[0].handler_name.as_deref(),
        Some("Core Media Audio")
    );
    assert!(
        mov.times.iter().any(
            |t| t.source == "mvhd.creation" && t.utc.as_deref() == Some("2026-03-01T10:00:00Z")
        )
    );
    assert_eq!(
        mov.bmff.as_ref().unwrap().top_level,
        ["ftyp", "wide", "mdat", "moov"]
    );
    let wav = read("tone-original.wav");
    let w = wav.wav.as_ref().unwrap();
    assert_eq!(
        (w.channels, w.sample_rate, w.bits_per_sample),
        (Some(1), Some(8000), Some(16))
    );
    assert!(wav.tags.iter().any(|t| t.key == "Originator"));
    let opus = read("voice-note.opus");
    let o = &opus.ogg.as_ref().unwrap().streams[0];
    assert_eq!(
        (o.codec.as_str(), o.pre_skip, o.vendor.as_deref()),
        ("Opus", Some(312), Some("libopus 1.4"))
    );
    assert_eq!(opus.ogg.as_ref().unwrap().crc_failures, 0);
    let mp3 = read("memo-lame.mp3");
    let m = mp3.mp3.as_ref().unwrap();
    assert_eq!((m.frames, m.vbr, m.sample_rate), (20, false, Some(44_100)));
    assert_eq!(
        m.xing.as_ref().unwrap().encoder.as_deref(),
        Some("LAME3.100")
    );
    assert_eq!(m.id3v2_version.as_deref(), Some("2.3.0"));
}
