//! RV AvAuth media layer: bounded, pure-Rust parsers for the audio and video containers the tool examines
//! (ISO BMFF — MP4, MOV, M4A, 3GP —, RIFF/WAVE, Ogg with Opus or Vorbis, MPEG audio layer III with ID3) and
//! the authenticity *signals* derived from what they read. Signals are never verdicts.
//!
//! Safety rules of this crate (tested in `tests/hostile.rs`):
//! - no `unsafe` code;
//! - no size read from the file is trusted: every read is checked against the real file length and a fixed
//!   cap before anything is allocated (see [`source::Source::read_vec`]);
//! - every loop over file structures has a hard limit (boxes, chunks, pages, frames, tags, nesting depth);
//! - a structure that does not fit is recorded as an [`model::Anomaly`], never a panic.
//!
//! ```text
//! bytes ──► sniff ──► bmff | riff | ogg | mp3 ──► MediaInfo ──► signals() ──► Signal list
//!                                                   └──────► reference::fingerprint() ──► compare()
//! ```

#![forbid(unsafe_code)]

pub mod bmff;
pub mod encoders;
pub mod model;
pub mod mp3;
pub mod ogg;
pub mod reference;
pub mod riff;
pub mod signals;
pub mod source;
pub mod util;

pub use model::{Container, MediaInfo, Signal};
pub use source::{FileSource, SliceSource, Source};

/// Identifies the container from the first bytes (content, never the extension).
pub fn sniff(head: &[u8]) -> Container {
    if head.len() >= 12
        && (&head[0..4] == b"RIFF" || &head[0..4] == b"RF64")
        && &head[8..12] == b"WAVE"
    {
        return Container::Wave;
    }
    if head.starts_with(b"OggS") {
        return Container::Ogg;
    }
    if head.len() >= 8 {
        let t = &head[4..8];
        if t == b"ftyp" {
            return Container::IsoBmff;
        }
        // QuickTime files without ftyp start directly with one of these boxes.
        let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        if matches!(t, b"moov" | b"mdat" | b"wide" | b"free" | b"skip" | b"pnot")
            && (size == 1 || size >= 8)
        {
            return Container::IsoBmff;
        }
    }
    if head.starts_with(b"ID3") {
        return Container::Mp3;
    }
    if let Some(h) = mp3::FrameHeader::parse(head)
        && h.layer == 3
    {
        let next = h.length as usize;
        // A single sync word is too weak: the next frame must start where this one ends.
        if head.len() < next + 4 || mp3::FrameHeader::parse(&head[next..]).is_some() {
            return Container::Mp3;
        }
    }
    Container::Unknown
}

/// Parses one file. Never panics; whatever cannot be read is recorded in `anomalies` and `notes`.
pub fn parse(src: &mut dyn Source) -> MediaInfo {
    let mut head = vec![0u8; 64.min(src.len() as usize)];
    let n = src.read_at(0, &mut head).unwrap_or(0);
    head.truncate(n);
    let container = sniff(&head);
    let mut info = MediaInfo::new(container, src.len());
    match container {
        Container::IsoBmff => bmff::parse(src, &mut info),
        Container::Wave => riff::parse(src, &mut info),
        Container::Ogg => ogg::parse(src, &mut info),
        Container::Mp3 => mp3::parse(src, &mut info),
        Container::Unknown => {}
    }
    encoders::classify_all(&mut info);
    info
}

/// Parses an in-memory buffer (tests, fuzzing).
pub fn parse_bytes(bytes: &[u8]) -> MediaInfo {
    parse(&mut SliceSource::new(bytes))
}
