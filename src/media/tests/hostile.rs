//! Hostile input: truncated files, oversized size fields, deep nesting, random bytes and thousands of
//! byte mutations of the samples. The parsers must never panic, and no single read (= allocation) may be
//! larger than the file itself or the 4 MiB structure cap.

use rv_avauth_media::model::limits::MAX_STRUCT_BYTES;
use rv_avauth_media::signals::{SignalCtx, signals};
use rv_avauth_media::{Source, parse};
use std::path::Path;

/// A source that records the largest single read.
struct Counting<'a> {
    data: &'a [u8],
    max_read: usize,
    reads: u64,
}

impl Source for Counting<'_> {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        self.max_read = self.max_read.max(buf.len());
        self.reads += 1;
        if offset >= self.data.len() as u64 {
            return Ok(0);
        }
        let s = offset as usize;
        let n = buf.len().min(self.data.len() - s);
        buf[..n].copy_from_slice(&self.data[s..s + n]);
        Ok(n)
    }
}

fn check(bytes: &[u8], what: &str) {
    let mut src = Counting {
        data: bytes,
        max_read: 0,
        reads: 0,
    };
    let info = parse(&mut src);
    assert!(
        src.max_read as u64 <= (bytes.len() as u64).max(64).min(MAX_STRUCT_BYTES.max(64)),
        "{what}: a read of {} bytes on a {}-byte file",
        src.max_read,
        bytes.len()
    );
    assert!(info.nodes.len() <= rv_avauth_media::model::limits::MAX_NODES);
    let _ = signals(
        &info,
        &SignalCtx {
            extension: Some("mp4".into()),
            fs_modified: Some(0),
            now: 1_800_000_000,
        },
    );
    let _ = rv_avauth_media::reference::fingerprint(&info);
    let _ = serde_json::to_string(&info).unwrap();
}

fn samples() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sample/evidence");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

#[test]
fn every_prefix_of_every_sample() {
    for (name, b) in samples() {
        let step = (b.len() / 300).max(1);
        let mut n = 0;
        while n <= b.len() {
            check(&b[..n], &format!("{name}[..{n}]"));
            n += step;
        }
    }
}

#[test]
fn thousands_of_byte_mutations() {
    let mut rng = Rng(0x5256_4156_4155_5448);
    let all = samples();
    let rounds: usize = std::env::var("RV_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6000);
    for round in 0..rounds {
        let (name, orig) = &all[round % all.len()];
        if orig.is_empty() {
            continue;
        }
        let mut b = orig.clone();
        let edits = 1 + (rng.next() % 8) as usize;
        for _ in 0..edits {
            let at = (rng.next() % b.len() as u64) as usize;
            match rng.next() % 4 {
                0 => b[at] = rng.next() as u8,
                1 => b[at] = 0xFF,
                2 => {
                    // A 32-bit size field set to something huge.
                    let v = [0xFFu8, 0xFF, 0xFF, (rng.next() as u8) | 0xF0];
                    for (i, x) in v.iter().enumerate() {
                        if at + i < b.len() {
                            b[at + i] = *x;
                        }
                    }
                }
                _ => {
                    let cut = at.max(1);
                    b.truncate(cut);
                }
            }
            if b.is_empty() {
                break;
            }
        }
        check(&b, &format!("{name} mutation {round}"));
    }
}

#[test]
fn random_bytes_with_valid_magics() {
    let mut rng = Rng(42);
    for magic in [
        &b"\0\0\0\x18ftypisom"[..],
        b"RIFF\xff\xff\xff\xffWAVE",
        b"RF64\xff\xff\xff\xffWAVE",
        b"OggS\0\x02",
        b"ID3\x04\0\0\x7f\x7f\x7f\x7f",
        b"\xff\xfb\x90\xc4",
        b"\0\0\0\x08moov",
    ] {
        for len in [0usize, 1, 7, 16, 100, 1000, 70_000] {
            let mut b = magic.to_vec();
            while b.len() < len {
                b.push(rng.next() as u8);
            }
            check(&b, &format!("magic {:?} len {len}", &magic[..4]));
        }
    }
}

#[test]
fn oversized_and_nested_structures() {
    // Largesize box claiming 2^63 bytes.
    let mut b = b"\0\0\0\x14ftypisom\0\0\0\0isom".to_vec();
    b.extend_from_slice(b"\0\0\0\x01mdat\x7f\xff\xff\xff\xff\xff\xff\xff");
    b.extend_from_slice(&[0u8; 32]);
    check(&b, "largesize");
    let info = rv_avauth_media::parse_bytes(&b);
    assert!(info.anomaly_count(rv_avauth_media::model::AnomalyKind::Truncated) >= 1);

    // 5000 nested moov boxes (sizes consistent): depth is capped, no stack overflow.
    let depth = 5000usize;
    let mut v = Vec::with_capacity(depth * 8 + 24);
    v.extend_from_slice(b"\0\0\0\x14ftypisom\0\0\0\0isom");
    for i in 0..depth {
        let size = ((depth - i) * 8) as u32;
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(b"moov");
    }
    check(&v, "nested");

    // stts declaring 4 billion entries in a 24-byte box.
    let stts = b"\0\0\0\x18sttsXXXX\xff\xff\xff\xff\0\0\0\x01\0\0\0\x01".to_vec();
    let mut f = b"\0\0\0\x14ftypisom\0\0\0\0isom".to_vec();
    let trak_payload = [stts.clone()].concat();
    let mut trak = ((8 + trak_payload.len()) as u32).to_be_bytes().to_vec();
    trak.extend_from_slice(b"trak");
    trak.extend_from_slice(&trak_payload);
    let mut moov = ((8 + trak.len()) as u32).to_be_bytes().to_vec();
    moov.extend_from_slice(b"moov");
    moov.extend_from_slice(&trak);
    f.extend_from_slice(&moov);
    check(&f, "stts count");

    // ID3v2 claiming 256 MiB, RIFF data chunk claiming 4 GiB, Ogg page with 255 full segments.
    let mut id3 = b"ID3\x03\0\0\x7f\x7f\x7f\x7f".to_vec();
    id3.extend_from_slice(b"TSSE\xff\xff\xff\xff\0\0\0abc");
    check(&id3, "id3 sizes");
    let mut wav =
        b"RIFF\x24\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0"
            .to_vec();
    wav.extend_from_slice(b"data\xff\xff\xff\xff\0\0");
    check(&wav, "wav data size");
    let mut ogg = b"OggS\0\x02\0\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\xff".to_vec();
    ogg.extend_from_slice(&[255u8; 255]);
    ogg.extend_from_slice(&[0u8; 1000]);
    check(&ogg, "ogg segments");
}
