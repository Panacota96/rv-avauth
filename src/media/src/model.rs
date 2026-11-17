//! The frozen data model of one parsed media file. Field names are part of the technical JSON
//! (`structure.json`, `details.media`): add fields, never rename them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Hard limits that keep a hostile file from using unbounded memory or time.
pub mod limits {
    /// Structure nodes (boxes, chunks, pages summarised, tag frames) kept in `MediaInfo::nodes`.
    pub const MAX_NODES: usize = 4000;
    /// ISO BMFF nesting depth.
    pub const MAX_DEPTH: usize = 16;
    /// Bytes read for one metadata structure (moov leaf box, tag, INFO chunk, comment packet).
    pub const MAX_STRUCT_BYTES: u64 = 4 * 1024 * 1024;
    /// Tags kept (each value capped at `MAX_TAG_VALUE` characters).
    pub const MAX_TAGS: usize = 512;
    pub const MAX_TAG_VALUE: usize = 512;
    /// RIFF chunks walked at the top level.
    pub const MAX_CHUNKS: usize = 10_000;
    /// Ogg pages walked.
    pub const MAX_PAGES: u64 = 2_000_000;
    /// MPEG audio frames walked (about 15 hours at 44.1 kHz).
    pub const MAX_FRAMES: u64 = 2_000_000;
    /// Bytes scanned forward to find the next frame or page after a sync loss.
    pub const MAX_RESYNC_SCAN: u64 = 1024 * 1024;
    /// Anomalies of one kind kept with their offsets (all are counted).
    pub const MAX_ANOMALIES_PER_KIND: usize = 50;
    /// Sample-table entries summed (stts).
    pub const MAX_TABLE_ENTRIES: u64 = 1_000_000;
}

/// Container family, identified from the content (never from the extension).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    IsoBmff,
    Wave,
    Ogg,
    Mp3,
    Unknown,
}

impl Container {
    pub fn name(&self) -> &'static str {
        match self {
            Container::IsoBmff => "ISO BMFF (MP4/MOV/M4A/3GP)",
            Container::Wave => "RIFF/WAVE",
            Container::Ogg => "Ogg",
            Container::Mp3 => "MPEG audio (MP3)",
            Container::Unknown => "unknown",
        }
    }
    pub fn key(&self) -> &'static str {
        match self {
            Container::IsoBmff => "iso_bmff",
            Container::Wave => "wave",
            Container::Ogg => "ogg",
            Container::Mp3 => "mp3",
            Container::Unknown => "unknown",
        }
    }
    /// Extensions normally used for this container.
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Container::IsoBmff => &[
                "mp4", "m4v", "m4a", "m4b", "m4p", "mov", "qt", "3gp", "3g2", "3gpp", "f4v",
                "heic", "heif", "avif", "mj2",
            ],
            Container::Wave => &["wav", "wave", "bwf", "rf64"],
            Container::Ogg => &["ogg", "oga", "ogv", "opus", "spx", "ogx"],
            Container::Mp3 => &["mp3", "mp2", "mpga"],
            Container::Unknown => &[],
        }
    }
    /// The container an extension claims, if it is a media extension this tool examines.
    pub fn from_extension(ext: &str) -> Option<Container> {
        let e = ext.to_ascii_lowercase();
        [
            Container::IsoBmff,
            Container::Wave,
            Container::Ogg,
            Container::Mp3,
        ]
        .into_iter()
        .find(|c| c.extensions().contains(&e.as_str()))
    }
}

/// One structure node (box, chunk, page run, tag frame) with its byte range.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub depth: u8,
    pub kind: String,
    pub offset: u64,
    pub size: u64,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub note: String,
}

/// ISO BMFF `ftyp`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Brand {
    pub major: String,
    pub minor_version: u32,
    pub compatible: Vec<String>,
}

/// One elementary stream (track, fmt chunk, logical Ogg stream, MPEG audio stream).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stream {
    pub index: u32,
    /// audio, video, text, metadata, other
    pub kind: String,
    /// Codec identifier: ISO BMFF sample-entry fourcc, WAVE format tag, Ogg codec name, MPEG layer.
    pub codec: String,
    pub handler_name: Option<String>,
    pub compressor_name: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub bits_per_sample: Option<u16>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub timescale: Option<u32>,
    /// Duration from the stream's own header (tkhd/mdhd, header fields), in milliseconds.
    pub duration_ms: Option<u64>,
    /// Number of samples or frames counted in the sample table / stream.
    pub samples: Option<u64>,
    pub bitrate_kbps: Option<u32>,
    pub language: Option<String>,
}

/// A time read from the file. `utc` is RFC 3339 UTC when the value converts to a plausible date.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TimeValue {
    /// e.g. mvhd.creation, tkhd[1].modification, mdhd[1].creation, tag ©day, bext.origination
    pub source: String,
    /// Raw value as stored (seconds since 1904 for ISO BMFF, text for tags).
    pub raw: String,
    /// Seconds since 1970-01-01 UTC; None when zero, unreadable or text without a time zone.
    pub unix: Option<i64>,
    pub utc: Option<String>,
    /// header (structural field) or tag (free metadata text)
    pub origin: String,
}

/// A metadata tag.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    /// ilst, udta, keys, ID3v2.3, ID3v1, APEv2, INFO, bext, OpusTags, VorbisComment
    pub source: String,
    pub key: String,
    pub value: String,
}

/// What kind of software or device an encoder string points to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderCategory {
    /// Camera, phone or recorder firmware / OS (e.g. com.apple.quicktime.software "17.1").
    Device,
    /// Recording app on a device (e.g. a voice recorder app).
    RecorderApp,
    /// Messaging app (re-encodes media it sends).
    Messenger,
    /// Codec library that ordinary recorders also use (LAME, libvorbis, libopus).
    EncoderLibrary,
    /// General transcoder or multiplexer (FFmpeg / Lavf, HandBrake).
    Transcoder,
    /// Audio or video editing software (Audacity, Adobe, iMovie…).
    Editor,
    /// Online conversion service.
    OnlineConverter,
    Unknown,
}

impl EncoderCategory {
    pub fn key(&self) -> &'static str {
        match self {
            EncoderCategory::Device => "device",
            EncoderCategory::RecorderApp => "recorder_app",
            EncoderCategory::Messenger => "messenger",
            EncoderCategory::EncoderLibrary => "encoder_library",
            EncoderCategory::Transcoder => "transcoder",
            EncoderCategory::Editor => "editor",
            EncoderCategory::OnlineConverter => "online_converter",
            EncoderCategory::Unknown => "unknown",
        }
    }
}

/// An encoder / writing-software string and how it is classified.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EncoderTrace {
    /// Where it was found: ilst ©too, udta ©swr, stsd compressor, ID3 TSSE, LAME tag, OpusTags vendor, INFO ISFT…
    pub source: String,
    pub value: String,
    /// Matched product label (e.g. "FFmpeg (Lavf)"), empty when unknown.
    pub label: String,
    pub category: EncoderCategory,
}

/// A duration, either declared by a header or computed from the stream.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DurationValue {
    /// mvhd, tkhd[1], mdhd[1], stts[1], wave header, data chunk, Xing frames, frames counted, ID3 TLEN, last granule
    pub source: String,
    pub ms: u64,
    /// declared or computed
    pub kind: String,
}

/// Structural problem kinds. Each maps to a finding in `signals.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyKind {
    /// A size field does not fit its parent or the file (box, chunk, tag).
    SizeInconsistent,
    /// Bytes after the logical end of the container.
    TrailingData,
    /// The last structure (box, chunk, page, frame) is cut off.
    Truncated,
    /// Bytes between frames or pages that are not part of the stream.
    SyncGap,
    /// Ogg page whose CRC-32 does not match.
    CrcMismatch,
    /// Ogg page sequence numbers jump.
    SequenceGap,
    /// Ogg granule position goes backwards.
    GranuleNotMonotonic,
    /// A new logical Ogg stream starts after another ended (chained streams).
    ChainedStreams,
    /// Sample rate, channels or MPEG version change inside one stream.
    ParametersChange,
    /// Header fields that contradict each other (WAVE byte rate vs block align…).
    FormatParameters,
    /// A structure is nested deeper than the parser follows, or a cap was reached.
    LimitReached,
}

impl AnomalyKind {
    pub fn key(&self) -> &'static str {
        match self {
            AnomalyKind::SizeInconsistent => "size_inconsistent",
            AnomalyKind::TrailingData => "trailing_data",
            AnomalyKind::Truncated => "truncated",
            AnomalyKind::SyncGap => "sync_gap",
            AnomalyKind::CrcMismatch => "crc_mismatch",
            AnomalyKind::SequenceGap => "sequence_gap",
            AnomalyKind::GranuleNotMonotonic => "granule_not_monotonic",
            AnomalyKind::ChainedStreams => "chained_streams",
            AnomalyKind::ParametersChange => "parameters_change",
            AnomalyKind::FormatParameters => "format_parameters",
            AnomalyKind::LimitReached => "limit_reached",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anomaly {
    pub kind: AnomalyKind,
    pub offset: u64,
    pub detail: String,
}

/// ISO BMFF specifics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BmffInfo {
    /// Top-level box order (fourccs), e.g. ["ftyp", "wide", "mdat", "moov"].
    pub top_level: Vec<String>,
    /// (offset, size) of every mdat box.
    pub mdat: Vec<(u64, u64)>,
    pub moov_offset: Option<u64>,
    pub moov_count: u32,
    /// moof boxes present (fragmented MP4).
    pub fragmented: bool,
    pub movie_timescale: Option<u32>,
    pub movie_duration: Option<u64>,
    pub edit_lists: u32,
    /// free / skip / wide boxes (count and bytes).
    pub free_boxes: u32,
    pub free_bytes: u64,
    /// Vendor-typical boxes or keys seen (apple_keys, android_keys, xyz_location, samsung_smta, uuid:<hex>…).
    pub vendor_traces: Vec<String>,
}

/// RIFF/WAVE specifics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WavInfo {
    pub rf64: bool,
    /// RIFF size field (+8 = the size the header claims for the file).
    pub riff_size: u64,
    pub format_tag: Option<u16>,
    pub channels: Option<u16>,
    pub sample_rate: Option<u32>,
    pub byte_rate: Option<u32>,
    pub block_align: Option<u16>,
    pub bits_per_sample: Option<u16>,
    pub data_offset: Option<u64>,
    pub data_size_declared: Option<u64>,
    pub data_size_present: Option<u64>,
    pub fact_samples: Option<u64>,
    /// Chunk ids in file order.
    pub chunks: Vec<String>,
}

/// One logical Ogg stream.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OggStream {
    pub serial: u32,
    pub codec: String,
    pub vendor: Option<String>,
    pub pages: u64,
    pub first_page_offset: u64,
    pub first_granule: Option<i64>,
    pub last_granule: Option<i64>,
    pub pre_skip: Option<u16>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub bos: bool,
    pub eos: bool,
    pub sequence_gaps: u64,
    pub crc_failures: u64,
}

/// Ogg specifics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OggInfo {
    pub pages: u64,
    pub crc_failures: u64,
    pub streams: Vec<OggStream>,
    pub chained: bool,
}

/// The VBR/CBR header in the first MPEG audio frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct XingHeader {
    /// Xing (VBR), Info (CBR, LAME) or VBRI (Fraunhofer)
    pub tag: String,
    pub frames: Option<u32>,
    pub bytes: Option<u32>,
    /// Encoder version string of the LAME tag (e.g. "LAME3.100", "Lavc60.3").
    pub encoder: Option<String>,
}

/// MP3 specifics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mp3Info {
    pub id3v2_version: Option<String>,
    pub id3v2_size: Option<u64>,
    pub id3v1: bool,
    pub apev2: bool,
    pub first_frame_offset: Option<u64>,
    pub frames: u64,
    pub frames_capped: bool,
    pub mpeg_version: Option<String>,
    pub layer: Option<u8>,
    pub sample_rate: Option<u32>,
    pub channel_mode: Option<String>,
    pub vbr: bool,
    pub min_bitrate_kbps: Option<u32>,
    pub max_bitrate_kbps: Option<u32>,
    pub padded_frames: u64,
    pub xing: Option<XingHeader>,
    pub sync_gaps: u64,
    pub skipped_bytes: u64,
    /// Where the audio frames end (before ID3v1 / APEv2).
    pub audio_end: Option<u64>,
}

/// Everything read from one file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub container: Container,
    pub file_size: u64,
    /// End of the last structure the container accounts for; bytes after it are trailing data.
    pub logical_end: u64,
    pub brand: Option<Brand>,
    pub nodes: Vec<Node>,
    pub nodes_truncated: bool,
    pub streams: Vec<Stream>,
    pub times: Vec<TimeValue>,
    pub tags: Vec<Tag>,
    pub tags_truncated: bool,
    pub encoders: Vec<EncoderTrace>,
    /// Device the metadata claims (make, model, software of a device), from tags.
    pub device_claims: Vec<Tag>,
    pub durations: Vec<DurationValue>,
    pub anomalies: Vec<Anomaly>,
    /// Count per anomaly kind (anomalies beyond the per-kind cap are counted, not listed).
    pub anomaly_counts: BTreeMap<String, u64>,
    pub notes: Vec<String>,
    pub bmff: Option<BmffInfo>,
    pub wav: Option<WavInfo>,
    pub ogg: Option<OggInfo>,
    pub mp3: Option<Mp3Info>,
}

impl MediaInfo {
    pub fn new(container: Container, file_size: u64) -> MediaInfo {
        MediaInfo {
            container,
            file_size,
            logical_end: 0,
            brand: None,
            nodes: vec![],
            nodes_truncated: false,
            streams: vec![],
            times: vec![],
            tags: vec![],
            tags_truncated: false,
            encoders: vec![],
            device_claims: vec![],
            durations: vec![],
            anomalies: vec![],
            anomaly_counts: BTreeMap::new(),
            notes: vec![],
            bmff: None,
            wav: None,
            ogg: None,
            mp3: None,
        }
    }

    pub fn node(&mut self, depth: usize, kind: &str, offset: u64, size: u64, note: String) {
        if self.nodes.len() >= limits::MAX_NODES {
            self.nodes_truncated = true;
            return;
        }
        self.nodes.push(Node {
            depth: depth.min(255) as u8,
            kind: kind.into(),
            offset,
            size,
            note,
        });
    }

    pub fn anomaly(&mut self, kind: AnomalyKind, offset: u64, detail: impl Into<String>) {
        let c = self
            .anomaly_counts
            .entry(kind.key().to_string())
            .or_insert(0);
        *c += 1;
        if (*c as usize) <= limits::MAX_ANOMALIES_PER_KIND {
            self.anomalies.push(Anomaly {
                kind,
                offset,
                detail: detail.into(),
            });
        }
    }

    pub fn anomaly_count(&self, kind: AnomalyKind) -> u64 {
        self.anomaly_counts.get(kind.key()).copied().unwrap_or(0)
    }

    pub fn first_anomaly(&self, kind: AnomalyKind) -> Option<&Anomaly> {
        self.anomalies.iter().find(|a| a.kind == kind)
    }

    pub fn tag(&mut self, source: &str, key: &str, value: &str) {
        let value = crate::util::clean_text(value);
        if value.is_empty() {
            return;
        }
        if self.tags.len() >= limits::MAX_TAGS {
            self.tags_truncated = true;
            return;
        }
        self.tags.push(Tag {
            source: source.into(),
            key: key.into(),
            value,
        });
    }

    pub fn encoder(&mut self, source: &str, value: &str) {
        let value = crate::util::clean_text(value);
        if value.is_empty()
            || self
                .encoders
                .iter()
                .any(|e| e.source == source && e.value == value)
        {
            return;
        }
        self.encoders.push(EncoderTrace {
            source: source.into(),
            value,
            label: String::new(),
            category: EncoderCategory::Unknown,
        });
    }

    pub fn time(&mut self, t: TimeValue) {
        if self.times.len() < limits::MAX_TAGS {
            self.times.push(t);
        }
    }

    pub fn duration(&mut self, source: &str, ms: u64, kind: &str) {
        if self.durations.len() < limits::MAX_TAGS {
            self.durations.push(DurationValue {
                source: source.into(),
                ms,
                kind: kind.into(),
            });
        }
    }

    pub fn note(&mut self, n: impl Into<String>) {
        if self.notes.len() < 100 {
            self.notes.push(n.into());
        }
    }
}

/// A signal for the core: a stable code worded in i18n, a severity and string parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub code: String,
    /// info, low, medium, high
    pub severity: String,
    pub params: BTreeMap<String, String>,
}

impl Signal {
    pub fn new(code: &str, severity: &str) -> Signal {
        Signal {
            code: code.into(),
            severity: severity.into(),
            params: BTreeMap::new(),
        }
    }
    pub fn with(mut self, k: &str, v: impl Into<String>) -> Signal {
        self.params.insert(k.into(), v.into());
        self
    }
}
