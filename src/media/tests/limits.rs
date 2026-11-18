//! A walk stopped by a parser limit (frames, pages, boxes) leaves the rest of the file unexamined: that is
//! PARSER_LIMIT_REACHED (info), never TRAILING_DATA, TRUNCATED_STREAM or a duration mismatch. The limits are
//! lowered here (the production caps need gigabyte files).

use rv_avauth_media::model::{AnomalyKind, Container, MediaInfo};
use rv_avauth_media::signals::{SignalCtx, signals};
use rv_avauth_media::{SliceSource, Source};
use std::path::Path;

fn sample(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/sample/evidence")
            .join(name),
    )
    .unwrap()
}

fn codes(info: &MediaInfo, ext: &str) -> Vec<String> {
    signals(
        info,
        &SignalCtx {
            extension: Some(ext.into()),
            fs_modified: None,
            now: 1_800_000_000,
        },
    )
    .into_iter()
    .map(|s| s.code)
    .collect()
}

fn assert_limit_only(info: &MediaInfo, ext: &str) {
    assert_eq!(
        info.anomaly_count(AnomalyKind::TrailingData),
        0,
        "{:?}",
        info.anomalies
    );
    assert_eq!(
        info.anomaly_count(AnomalyKind::Truncated),
        0,
        "{:?}",
        info.anomalies
    );
    assert!(info.anomaly_count(AnomalyKind::LimitReached) >= 1);
    assert_eq!(info.logical_end, info.file_size);
    let c = codes(info, ext);
    assert!(c.contains(&"PARSER_LIMIT_REACHED".to_string()), "{c:?}");
    for bad in ["TRAILING_DATA", "TRUNCATED_STREAM", "DURATION_MISMATCH"] {
        assert!(!c.contains(&bad.to_string()), "{bad} in {c:?}");
    }
}

#[test]
fn mp3_frame_cap_is_a_parser_limit_not_trailing_data() {
    // memo-lame.mp3: ID3v2, Info frame (20 frames declared), 20 audio frames; walk only 5.
    let b = sample("memo-lame.mp3");
    let mut src = SliceSource::new(&b);
    let mut info = MediaInfo::new(Container::Mp3, src.len());
    rv_avauth_media::mp3::parse_with_limit(&mut src, &mut info, 5);
    let m = info.mp3.as_ref().unwrap();
    assert!(m.frames_capped);
    assert_eq!(m.frames, 5);
    assert_limit_only(&info, "mp3");
    // The same file walked in full has neither the limit nor trailing data.
    let full = rv_avauth_media::parse_bytes(&b);
    assert_eq!(full.anomaly_count(AnomalyKind::LimitReached), 0);
    assert!(!codes(&full, "mp3").contains(&"PARSER_LIMIT_REACHED".to_string()));
}

#[test]
fn ogg_page_cap_does_not_claim_a_missing_end_of_stream() {
    let b = sample("voice-note.opus");
    let mut src = SliceSource::new(&b);
    let mut info = MediaInfo::new(Container::Ogg, src.len());
    rv_avauth_media::ogg::parse_with_limit(&mut src, &mut info, 3);
    assert_eq!(info.ogg.as_ref().unwrap().pages, 3);
    assert_limit_only(&info, "opus");
}

#[test]
fn bmff_box_cap_does_not_turn_the_rest_into_trailing_data() {
    // camera-original.mov: ftyp, wide, mdat, moov at the top level; walk 2 boxes per parent.
    let b = sample("camera-original.mov");
    let mut src = SliceSource::new(&b);
    let mut info = MediaInfo::new(Container::IsoBmff, src.len());
    rv_avauth_media::bmff::parse_with_limit(&mut src, &mut info, 2);
    assert_limit_only(&info, "mov");
}
