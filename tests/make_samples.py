#!/usr/bin/env python3
"""Generates the synthetic sample evidence of RV AvAuth, byte by byte, with the Python standard library only.

  python tests/make_samples.py            writes examples/sample/evidence/, examples/sample/reference/,
                                          examples/sample/expected_findings.json and expected_hashes.csv

Every file is a SYNTHETIC TEST SAMPLE made by this script (deterministic: fixed dates, fixed content):
- audio is a 440 Hz sine tone or silence-like filler bytes — no recording, no voice, no person;
- MP4/MOV boxes, WAVE chunks, Ogg pages and MP3 frames are written by the functions below;
- software names (Lavf, Audacity, Adobe Premiere, online-audio-converter.com, LAME, libopus, libVorbis) are
  written as plain strings only so the encoder classification can be exercised; no third-party software
  produced these files.
The expected findings are the ground truth the end-to-end test (src/cli/tests/samples.rs) checks: each file
is built to trigger the listed codes and nothing else. Hashes are computed here with hashlib (and blake3 when
installed), independently of the tool.
"""

from __future__ import annotations

import csv
import hashlib
import json
import math
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SAMPLE = ROOT / "examples" / "sample"
EVIDENCE = SAMPLE / "evidence"
REFERENCE = SAMPLE / "reference"

T0 = 1_772_359_200  # 2026-03-01T10:00:00Z
MAC = 2_082_844_800  # seconds between 1904-01-01 and 1970-01-01
DAY = 86_400


# ------------------------------------------------------------------ audio content


def sine_pcm16(seconds: float = 0.5, rate: int = 8000, freq: float = 440.0, amp: int = 8000) -> bytes:
    n = int(seconds * rate)
    return b"".join(struct.pack("<h", int(round(amp * math.sin(2 * math.pi * freq * i / rate)))) for i in range(n))


def filler(n: int, seed: bytes) -> bytes:
    """Deterministic non-zero bytes (SHA-256 chain)."""
    out = b""
    h = seed
    while len(out) < n:
        h = hashlib.sha256(h).digest()
        out += h
    return out[:n]


# ------------------------------------------------------------------ RIFF/WAVE


def chunk(cid: bytes, payload: bytes) -> bytes:
    pad = b"\0" if len(payload) % 2 else b""
    return cid + struct.pack("<I", len(payload)) + payload + pad


def fmt_pcm(channels=1, rate=8000, bits=16, byte_rate=None, block_align=None) -> bytes:
    ba = block_align if block_align is not None else channels * bits // 8
    br = byte_rate if byte_rate is not None else rate * ba
    return chunk(b"fmt ", struct.pack("<HHIIHH", 1, channels, rate, br, ba, bits))


def info_list(items: list[tuple[bytes, str]]) -> bytes:
    body = b"INFO"
    for k, v in items:
        body += chunk(k, v.encode() + b"\0")
    return chunk(b"LIST", body)


def bext(originator: str, date: str, time: str) -> bytes:
    def field(s: str, n: int) -> bytes:
        return s.encode("latin-1")[:n].ljust(n, b"\0")

    body = (field("Synthetic 440 Hz test tone", 256) + field(originator, 32) + field("RV-0001", 32)
            + field(date, 10) + field(time, 8) + struct.pack("<Q", 0) + struct.pack("<H", 1)
            + b"\0" * 64 + b"\0" * 10 + b"\0" * 180)
    assert len(body) == 602
    return chunk(b"bext", body)


def riff(chunks: bytes, size_field: int | None = None) -> bytes:
    size = 4 + len(chunks) if size_field is None else size_field
    return b"RIFF" + struct.pack("<I", size) + b"WAVE" + chunks


# ------------------------------------------------------------------ ISO BMFF


def box(t: bytes, payload: bytes = b"", size: int | None = None) -> bytes:
    return struct.pack(">I", 8 + len(payload) if size is None else size) + t + payload


def full(t: bytes, version: int, flags: int, payload: bytes) -> bytes:
    return box(t, bytes([version]) + flags.to_bytes(3, "big") + payload)


MATRIX = struct.pack(">9I", 0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x40000000)


def mvhd(c: int, m: int, timescale: int, duration: int) -> bytes:
    return full(b"mvhd", 0, 0, struct.pack(">IIII", c, m, timescale, duration) + struct.pack(">IH", 0x10000, 0x100)
                + b"\0" * 10 + MATRIX + b"\0" * 24 + struct.pack(">I", 2))


def tkhd(c: int, m: int, duration: int) -> bytes:
    return full(b"tkhd", 0, 7, struct.pack(">IIIII", c, m, 1, 0, duration) + b"\0" * 8
                + struct.pack(">hhhH", 0, 0, 0x100, 0) + MATRIX + struct.pack(">II", 0, 0))


def mdhd(c: int, m: int, timescale: int, duration: int) -> bytes:
    lang = ((ord("u") - 0x60) << 10) | ((ord("n") - 0x60) << 5) | (ord("d") - 0x60)
    return full(b"mdhd", 0, 0, struct.pack(">IIIIHH", c, m, timescale, duration, lang, 0))


def hdlr(handler: bytes, name: str, mdir: bool = False) -> bytes:
    return full(b"hdlr", 0, 0, struct.pack(">I", 0) + handler + (b"appl" if mdir else b"\0" * 4) + b"\0" * 8
                + name.encode() + b"\0")


def stsd_audio(fmt: bytes, vendor: bytes, rate: int = 8000) -> bytes:
    entry = (b"\0" * 6 + struct.pack(">H", 1) + struct.pack(">HH", 0, 0) + vendor
             + struct.pack(">HHHHI", 1, 16, 0, 0, rate << 16))
    return full(b"stsd", 0, 0, struct.pack(">I", 1) + box(fmt, entry))


def stbl(fmt: bytes, vendor: bytes, samples: int, chunk_offset: int) -> bytes:
    return box(b"stbl", stsd_audio(fmt, vendor)
               + full(b"stts", 0, 0, struct.pack(">III", 1, samples, 1))
               + full(b"stsc", 0, 0, struct.pack(">IIII", 1, 1, samples, 1))
               + full(b"stsz", 0, 0, struct.pack(">II", 2, samples))
               + full(b"stco", 0, 0, struct.pack(">II", 1, chunk_offset)))


def audio_trak(c_track: int, c_media: int, m: int, track_dur: int, media_ts: int, media_dur: int, handler_name: str,
               fmt: bytes, vendor: bytes, samples: int, chunk_offset: int) -> bytes:
    minf = box(b"minf", full(b"smhd", 0, 0, b"\0" * 4)
               + box(b"dinf", full(b"dref", 0, 0, struct.pack(">I", 1) + full(b"url ", 0, 1, b"")))
               + stbl(fmt, vendor, samples, chunk_offset))
    mdia = box(b"mdia", mdhd(c_media, m, media_ts, media_dur) + hdlr(b"soun", handler_name) + minf)
    return box(b"trak", tkhd(c_track, m, track_dur) + mdia)


def data_atom(value: str) -> bytes:
    return box(b"data", struct.pack(">II", 1, 0) + value.encode())


def qt_meta(keys: list[tuple[str, str]]) -> bytes:
    """QuickTime metadata (meta without version/flags): hdlr mdta, keys, ilst indexed by key number."""
    k = full(b"keys", 0, 0, struct.pack(">I", len(keys))
             + b"".join(struct.pack(">I", 8 + len(n)) + b"mdta" + n.encode() for n, _ in keys))
    items = b"".join(box(struct.pack(">I", i + 1), data_atom(v)) for i, (_, v) in enumerate(keys))
    return box(b"meta", hdlr(b"mdta", "") + k + box(b"ilst", items))


def itunes_udta(items: list[tuple[bytes, str]]) -> bytes:
    ilst = box(b"ilst", b"".join(box(t, data_atom(v)) for t, v in items))
    return box(b"udta", full(b"meta", 0, 0, hdlr(b"mdir", "", mdir=True) + ilst))


def qt_text(t: bytes, value: str) -> bytes:
    v = value.encode()
    return box(t, struct.pack(">HH", len(v), 0) + v)


def mov_file(*, brand=b"qt  ", compat=(b"qt  ",), times=(T0, T0, T0, T0), keys=None, udta=b"",
             handler="Core Media Audio", vendor=b"appl", moov_first=False, fmt=b"sowt", extra_mdat=False,
             movie=(1000, 500), track_dur=500, media=(8000, 4000), samples=4000, free=False) -> bytes:
    """times = (movie creation, movie modification, track creation, media creation) as Unix seconds (0 = unset)."""
    mac = [t + MAC if t else 0 for t in times]
    ftyp = box(b"ftyp", brand + struct.pack(">I", 0) + b"".join(compat))
    pcm = sine_pcm16()
    head = ftyp + (box(b"wide") if not moov_first else b"")

    def moov(chunk_offset: int) -> bytes:
        trak = audio_trak(mac[2], mac[3], mac[1], track_dur, media[0], media[1], handler, fmt, vendor, samples,
                          chunk_offset)
        meta = qt_meta(keys) if keys else b""
        return box(b"moov", mvhd(mac[0], mac[1], movie[0], movie[1]) + trak + meta + udta)

    if moov_first:
        size = len(moov(0))
        pre = len(head) + size + (8 if free else 0)
        out = head + moov(pre + 8) + (box(b"free") if free else b"") + box(b"mdat", pcm)
    else:
        out = head + box(b"mdat", pcm)
        out += moov(len(head) + 8)
    if extra_mdat:
        out += box(b"mdat", filler(64, b"second mdat"))
    return out


APPLE_KEYS = [
    ("com.apple.quicktime.make", "Rakvor"),
    ("com.apple.quicktime.model", "Synthetic Recorder 1"),
    ("com.apple.quicktime.software", "1.0"),
    ("com.apple.quicktime.creationdate", "2026-03-01T11:00:00+0100"),
]


# ------------------------------------------------------------------ Ogg


def ogg_crc(data: bytes) -> int:
    r = 0
    for b in data:
        r ^= b << 24
        for _ in range(8):
            r = ((r << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if r & 0x80000000 else (r << 1) & 0xFFFFFFFF
    return r


def ogg_page(htype: int, granule: int, serial: int, seq: int, packets: list[bytes], corrupt: bool = False) -> bytes:
    segs = b""
    body = b""
    for p in packets:
        n = len(p)
        segs += b"\xff" * (n // 255) + bytes([n % 255])
        body += p
    hdr = b"OggS" + bytes([0, htype]) + struct.pack("<qIII", granule, serial, seq, 0) + bytes([len(segs)]) + segs
    page = hdr + body
    crc = ogg_crc(page)
    page = page[:22] + struct.pack("<I", crc) + page[26:]
    if corrupt:
        page = page[:-1] + bytes([page[-1] ^ 0x5A])
    return page


def opus_head() -> bytes:
    return b"OpusHead" + struct.pack("<BBHIhB", 1, 1, 312, 8000, 0, 0)


def opus_tags(vendor: str, comments: list[str] = ()) -> bytes:
    v = vendor.encode()
    out = b"OpusTags" + struct.pack("<I", len(v)) + v + struct.pack("<I", len(comments))
    for c in comments:
        out += struct.pack("<I", len(c.encode())) + c.encode()
    return out


SILENT_OPUS = b"\xf8\xff\xfe"  # a 20 ms Opus packet (TOC + filler); never decoded by the tool


def opus_stream(vendor: str, serial: int, plan: list[tuple[int, int, bool]]) -> bytes:
    """plan: (sequence number, granule, corrupt CRC) of each audio page; the last one carries EOS."""
    out = ogg_page(2, 0, serial, 0, [opus_head()]) + ogg_page(0, 0, serial, 1, [opus_tags(vendor)])
    for i, (seq, gran, bad) in enumerate(plan):
        eos = 4 if i == len(plan) - 1 else 0
        out += ogg_page(eos, gran, serial, seq, [SILENT_OPUS] * 5, corrupt=bad)
    return out


def vorbis_stream(serial: int, vendor: str) -> bytes:
    ident = b"\x01vorbis" + struct.pack("<IBIiiiBB", 0, 1, 8000, 0, 32000, 0, 0xB8, 1)
    v = vendor.encode()
    comm = b"\x03vorbis" + struct.pack("<I", len(v)) + v + struct.pack("<I", 0) + b"\x01"
    setup = b"\x05vorbis" + filler(40, b"vorbis setup " + bytes([serial & 0xFF]))
    audio = [filler(30, b"vorbis audio %d" % i) for i in range(3)]
    return (ogg_page(2, 0, serial, 0, [ident]) + ogg_page(0, 0, serial, 1, [comm, setup])
            + ogg_page(0, 2048, serial, 2, audio[:2]) + ogg_page(4, 4000, serial, 3, audio[2:]))


# ------------------------------------------------------------------ MP3


def id3v2(major: int, frames: list[tuple[str, str]]) -> bytes:
    body = b""
    for fid, text in frames:
        data = b"\x03" + text.encode() if major == 4 else b"\x00" + text.encode("latin-1")
        size = len(data)
        sz = (bytes([(size >> 21) & 0x7F, (size >> 14) & 0x7F, (size >> 7) & 0x7F, size & 0x7F]) if major == 4
              else struct.pack(">I", size))
        body += fid.encode() + sz + b"\0\0" + data
    body += b"\0" * 32  # padding
    n = len(body)
    return b"ID3" + bytes([major, 0, 0]) + bytes([(n >> 21) & 0x7F, (n >> 14) & 0x7F, (n >> 7) & 0x7F, n & 0x7F]) + body


MP1_HDR = b"\xff\xfb\x90\xc4"  # MPEG-1 layer III, 128 kb/s, 44.1 kHz, mono: 417 bytes
MP2_HDR = b"\xff\xf3\x80\xc4"  # MPEG-2 layer III, 64 kb/s, 22.05 kHz, mono: 208 bytes


def frame(hdr: bytes, length: int) -> bytes:
    return hdr + b"\0" * (length - 4)


def info_frame(tag: bytes, frames: int, encoder: str) -> bytes:
    """First frame carrying a Xing/Info header (mono MPEG-1: side info 17 bytes) and an encoder string."""
    body = b"\0" * 17 + tag + struct.pack(">II", 3, frames) + struct.pack(">I", frames * 417) + encoder.encode()[:9]
    f = MP1_HDR + body
    return f + b"\0" * (417 - len(f))


# ------------------------------------------------------------------ the samples


def samples() -> dict[str, tuple[bytes, list[str]]]:
    pcm = sine_pcm16()
    s: dict[str, tuple[bytes, list[str]]] = {}
    rec = bext("RV-REC-01 synthetic recorder", "2026-03-01", "10:00:00")
    info_ok = info_list([(b"ICRD", "2026-03-01"), (b"INAM", "Synthetic tone")])

    s["tone-original.wav"] = (riff(fmt_pcm() + rec + info_ok + chunk(b"data", pcm)),
                              ["DEVICE_CLAIM", "ENCODER_IDENTIFIED"])
    s["tone-edited-audacity.wav"] = (
        riff(fmt_pcm() + rec + info_list([(b"ICRD", "2026-03-01"), (b"ISFT", "Audacity 3.4.2")]) + chunk(b"data", pcm)),
        ["DEVICE_CLAIM", "EDITING_SOFTWARE_TRACE", "ENCODER_CONFLICTS_WITH_DEVICE_CLAIM", "ENCODER_IDENTIFIED"])
    full_wav = fmt_pcm() + chunk(b"data", pcm)
    half = fmt_pcm() + b"data" + struct.pack("<I", len(pcm)) + pcm[: len(pcm) // 2]
    s["tone-truncated.wav"] = (riff(half, size_field=4 + len(full_wav)),
                               ["DURATION_MISMATCH", "METADATA_ABSENT", "TRUNCATED_STREAM"])
    s["tone-appended.wav"] = (riff(full_wav) + filler(2048, b"appended"), ["METADATA_ABSENT", "TRAILING_DATA"])
    s["bad-fmt.wav"] = (riff(fmt_pcm(byte_rate=8000) + chunk(b"data", pcm)),
                        ["FORMAT_PARAMETERS_INCONSISTENT", "METADATA_ABSENT"])
    s["renamed-wave.mp3"] = (riff(full_wav), ["CONTAINER_EXTENSION_MISMATCH", "METADATA_ABSENT"])
    s["empty.wav"] = (b"", ["EMPTY_FILE", "MEDIA_NOT_RECOGNISED"])
    s["notes.txt"] = (b"Synthetic sample notes for RV AvAuth. Not evidence.\n", [])

    apple = ["DEVICE_CLAIM", "ENCODER_IDENTIFIED", "MOOV_AFTER_MDAT", "VENDOR_TRACES"]
    s["camera-original.mov"] = (mov_file(keys=APPLE_KEYS), apple)
    s["camera-renamed.mp4"] = (mov_file(keys=APPLE_KEYS, times=(T0 + 60,) * 4), apple + ["BRAND_EXTENSION_MISMATCH"])
    edited_keys = APPLE_KEYS[:3] + [("com.apple.quicktime.creationdate", "2026-02-19T10:00:00Z")]
    s["camera-edited.mov"] = (
        mov_file(keys=edited_keys, times=(T0, T0 + 3 * DAY, T0 + DAY, T0),
                 udta=box(b"udta", qt_text(b"\xa9too", "Adobe Premiere Pro 2024"))),
        apple + ["EDITING_SOFTWARE_TRACE", "ENCODER_CONFLICTS_WITH_DEVICE_CLAIM", "MEDIA_MODIFIED_AFTER_CREATION",
                 "MEDIA_TIMES_INCONSISTENT", "TAG_TIME_DIFFERS_FROM_HEADER"])
    s["voice-reencoded.m4a"] = (
        mov_file(brand=b"isom", compat=(b"isom", b"iso2", b"mp41"), times=(0, 0, 0, 0), handler="SoundHandler",
                 vendor=b"\0\0\0\0", fmt=b"mp4a", moov_first=True, free=True,
                 udta=itunes_udta([(b"\xa9too", "Lavf60.16.100")])),
        ["ENCODER_IDENTIFIED", "MEDIA_TIMES_ZERO", "TRANSCODER_TRACE"])
    s["duration-mismatch.mp4"] = (
        mov_file(brand=b"mp42", compat=(b"mp42", b"isom"), handler="", vendor=b"\0\0\0\0", fmt=b"mp4a",
                 moov_first=True, movie=(1000, 10_000), track_dur=10_000, media=(8000, 80_000), extra_mdat=True),
        ["DURATION_MISMATCH", "METADATA_ABSENT", "MULTIPLE_MDAT"])
    # A tkhd whose size field runs 200 bytes past its trak.
    tk = tkhd(T0 + MAC, T0 + MAC, 500)
    tk = struct.pack(">I", len(tk) + 200) + tk[4:]
    moov = box(b"moov", mvhd(T0 + MAC, T0 + MAC, 1000, 500) + box(b"trak", tk) + b"\0" * 200)
    s["broken-box.mp4"] = (box(b"ftyp", b"isom" + struct.pack(">I", 512) + b"isom") + moov + box(b"mdat", b"\0" * 64),
                           ["METADATA_ABSENT", "STRUCTURE_SIZE_INCONSISTENT"])

    good = [(2, 4800, False), (3, 9600, False), (4, 14400, False)]
    s["voice-note.opus"] = (opus_stream("libopus 1.4", 0x52564131, good), ["ENCODER_IDENTIFIED"])
    s["voice-note-spliced.opus"] = (
        opus_stream("Lavf60.16.100", 0x52564132, [(2, 4800, False), (3, 9600, False), (5, 7200, False),
                                                  (6, 12000, True), (7, 14400, False)]),
        ["ENCODER_IDENTIFIED", "OGG_CRC_MISMATCH", "OGG_GRANULE_NOT_MONOTONIC", "OGG_PAGE_SEQUENCE_GAP",
         "TRANSCODER_TRACE"])
    s["voice-note-truncated.opus"] = (opus_stream("libopus 1.4", 0x52564133, good)[:-20],
                                      ["ENCODER_IDENTIFIED", "TRUNCATED_STREAM"])
    xiph = "Xiph.Org libVorbis I 20200704 (Reducing Environment)"
    s["chained.ogg"] = (vorbis_stream(0x52564134, xiph) + vorbis_stream(0x52564135, xiph),
                        ["ENCODER_IDENTIFIED", "OGG_CHAINED_STREAMS"])

    s["memo-lame.mp3"] = (id3v2(3, [("TIT2", "Synthetic memo"), ("TSSE", "LAME 3.100")])
                          + info_frame(b"Info", 20, "LAME3.100") + frame(MP1_HDR, 417) * 20,
                          ["ENCODER_IDENTIFIED"])
    s["memo-converted.mp3"] = (
        id3v2(4, [("TSSE", "Lavf60.16.100"), ("TENC", "online-audio-converter.com")])
        + info_frame(b"Xing", 40, "Lavc60.31") + frame(MP1_HDR, 417) * 20,
        ["DURATION_MISMATCH", "ENCODER_IDENTIFIED", "ONLINE_CONVERTER_TRACE", "TRANSCODER_TRACE"])
    s["memo-spliced.mp3"] = (frame(MP1_HDR, 417) * 10 + b"\x55" * 300 + frame(MP2_HDR, 208) * 10,
                             ["FRAME_SYNC_GAPS", "METADATA_ABSENT", "STREAM_PARAMETERS_CHANGE"])
    s["memo-truncated.mp3"] = (frame(MP1_HDR, 417) * 10 + frame(MP1_HDR, 417)[:200],
                               ["METADATA_ABSENT", "TRUNCATED_STREAM"])
    return s


def digests(data: bytes) -> dict[str, str]:
    d = {a: hashlib.new(a, data).hexdigest() for a in ("md5", "sha1", "sha256", "sha512", "sha3_256")}
    try:
        import blake3  # type: ignore

        d["blake3"] = blake3.blake3(data).hexdigest()
    except ImportError:
        d["blake3"] = ""
    return d


def main() -> int:
    EVIDENCE.mkdir(parents=True, exist_ok=True)
    REFERENCE.mkdir(parents=True, exist_ok=True)
    for old in EVIDENCE.iterdir():
        old.unlink()
    s = samples()
    expected = {}
    rows = []
    for name in sorted(s):
        data, codes = s[name]
        (EVIDENCE / name).write_bytes(data)
        expected[name] = sorted(set(codes))
        d = digests(data)
        rows.append([name, len(data), d["md5"], d["sha1"], d["sha256"], d["sha512"], d["sha3_256"], d["blake3"]])
    ref = mov_file(keys=APPLE_KEYS, times=(T0 - DAY,) * 4)
    (REFERENCE / "camera-reference.mov").write_bytes(ref)
    (SAMPLE / "expected_findings.json").write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n",
                                                    encoding="utf-8", newline="\n")
    with open(SAMPLE / "expected_hashes.csv", "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh, lineterminator="\n")
        w.writerow(["file", "size", "md5", "sha1", "sha256", "sha512", "sha3-256", "blake3"])
        w.writerows(rows)
    print(f"{len(s)} samples + 1 reference written to {SAMPLE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
