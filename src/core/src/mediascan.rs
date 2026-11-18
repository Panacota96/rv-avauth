//! In-process media analysis of one item (MP4/MOV/M4A/3GP, WAV, Ogg/Opus/Vorbis, MP3): re-hash the evidence
//! in a second read-only pass, parse its container with the bounded parsers of `rv_avauth_media`, turn what
//! they read into findings, compare with the reference files, and write the derived files into the item
//! folder (listed so `run.rs` hashes and audits every one). A worker process runs exactly this code (see
//! `worker.rs`).
//!
//! Outputs per item: `structure.json` (the full parsed model: box/chunk/page tree with offsets, streams,
//! times, durations, anomalies), `metadata.json` (every tag, encoder string, device claim and time) and,
//! with `--reference`, `reference-comparison.json`. The report summaries (`media.*`) go into `details`.

use crate::analyzer::{Analysis, AnalyzeCtx, Finding, OutputFile, Severity, analyze};
use rv_avauth_media::model::{Container, MediaInfo};
use rv_avauth_media::signals::SignalCtx;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;

/// Rows of each list shown in the reports (the JSON outputs keep everything the parser kept).
pub const REPORT_ROWS: usize = 40;
/// Structure nodes shown in the reports.
pub const REPORT_TREE_ROWS: usize = 120;

/// ISO BMFF, RIFF/WAVE, Ogg or MP3 by content, or a media extension (a damaged header is still examined).
pub fn is_media_candidate(file_name: &str, head: &[u8]) -> bool {
    rv_avauth_media::sniff(head) != Container::Unknown
        || Path::new(file_name)
            .extension()
            .is_some_and(|e| Container::from_extension(&e.to_string_lossy()).is_some())
}

/// Writes a new file under the item folder (never overwrites) and lists it.
pub struct Writer<'a> {
    pub dir: &'a Path,
    pub outputs: Vec<OutputFile>,
    pub errors: Vec<Value>,
}

impl Writer<'_> {
    pub fn write(&mut self, rel: &str, kind: &str, data: &[u8]) -> bool {
        let p = self.dir.join(rel);
        let res = p
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::File::create_new(&p))
            .and_then(|mut f| f.write_all(data).and_then(|_| f.sync_all()));
        match res {
            Ok(()) => {
                self.outputs.push(OutputFile {
                    rel_path: rel.into(),
                    kind: kind.into(),
                });
                true
            }
            Err(e) => {
                // A half-written file is still listed (and therefore hashed and audited), once.
                if p.is_file() && !self.outputs.iter().any(|o| o.rel_path == rel) {
                    self.outputs.push(OutputFile {
                        rel_path: rel.into(),
                        kind: kind.into(),
                    });
                }
                self.errors
                    .push(json!({"file": rel, "kind": kind, "error": e.to_string()}));
                false
            }
        }
    }
}

fn severity(s: &str) -> Severity {
    match s {
        "high" => Severity::High,
        "medium" => Severity::Medium,
        "low" => Severity::Low,
        _ => Severity::Info,
    }
}

/// A media-layer signal as a finding (codes and parameters are worded in i18n).
pub fn finding(sig: &rv_avauth_media::Signal) -> Finding {
    let mut f = Finding::new(&sig.code, severity(&sig.severity));
    for (k, v) in &sig.params {
        f = f.with(k, v.clone());
    }
    f
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn opt<T: ToString>(v: &Option<T>) -> String {
    v.as_ref()
        .map(|x| x.to_string())
        .unwrap_or_else(|| "—".into())
}

/// `{"rows": [...first n...], "more": count beyond n}`.
fn capped(rows: Vec<Value>, n: usize) -> Value {
    let more = rows.len().saturating_sub(n);
    json!({"rows": rows.into_iter().take(n).collect::<Vec<_>>(), "more": more})
}

/// The report view of a parsed file: small, strings and integers only.
pub fn media_summary(info: &MediaInfo) -> Value {
    let mut overview = vec![
        json!({"label": "container", "value": info.container.name()}),
        json!({"label": "file_size", "value": info.file_size.to_string()}),
        json!({"label": "logical_end", "value": info.logical_end.to_string()}),
    ];
    if let Some(b) = &info.brand {
        overview.push(json!({"label": "major_brand", "value": format!("{} (minor {})", b.major, b.minor_version)}));
        overview.push(json!({"label": "compatible_brands", "value": b.compatible.join(", ")}));
    }
    if let Some(b) = &info.bmff {
        overview.push(json!({"label": "top_level_boxes", "value": b.top_level.join(" > ")}));
        overview.push(json!({"label": "mdat", "value": b.mdat.iter().map(|(o, s)| format!("@{} ({} bytes)", o, s)).collect::<Vec<_>>().join(", ")}));
        overview.push(json!({"label": "moov_offset", "value": opt(&b.moov_offset)}));
        overview.push(json!({"label": "fragmented", "value": b.fragmented.to_string()}));
        overview.push(json!({"label": "edit_lists", "value": b.edit_lists.to_string()}));
        overview.push(json!({"label": "free_boxes", "value": format!("{} ({} bytes)", b.free_boxes, b.free_bytes)}));
        if !b.vendor_traces.is_empty() {
            overview.push(json!({"label": "vendor_traces", "value": b.vendor_traces.join(", ")}));
        }
    }
    if let Some(w) = &info.wav {
        overview.push(json!({"label": "riff_form", "value": format!("{}{}", if w.rf64 { "RF64 " } else { "RIFF " }, w.riff_size)}));
        overview.push(json!({"label": "chunks", "value": w.chunks.join(" > ")}));
        overview.push(json!({"label": "wave_format", "value": format!("tag {} · {} ch · {} Hz · {} bit · byte rate {} · block align {}",
            opt(&w.format_tag), opt(&w.channels), opt(&w.sample_rate), opt(&w.bits_per_sample), opt(&w.byte_rate), opt(&w.block_align))}));
        overview.push(json!({"label": "data_chunk", "value": format!("@{} declared {} present {}", opt(&w.data_offset), opt(&w.data_size_declared), opt(&w.data_size_present))}));
    }
    if let Some(o) = &info.ogg {
        overview.push(json!({"label": "ogg_pages", "value": format!("{} pages, {} CRC failures, chained {}", o.pages, o.crc_failures, o.chained)}));
        for s in &o.streams {
            overview.push(json!({"label": format!("ogg_stream {:08x}", s.serial), "value": format!(
                "{} · vendor {} · pages {} · granule {}–{} · pre-skip {} · BOS {} · EOS {} · sequence gaps {} · CRC failures {}",
                s.codec, opt(&s.vendor), s.pages, opt(&s.first_granule), opt(&s.last_granule), opt(&s.pre_skip),
                s.bos, s.eos, s.sequence_gaps, s.crc_failures)}));
        }
    }
    if let Some(m) = &info.mp3 {
        overview.push(json!({"label": "id3", "value": format!("ID3v2 {} ({} bytes) · ID3v1 {} · APEv2 {}", opt(&m.id3v2_version), opt(&m.id3v2_size), m.id3v1, m.apev2)}));
        overview.push(json!({"label": "mpeg_frames", "value": format!(
            "{} {} layer {} · {} Hz · {} · {} frames{} · {} · bitrate {}–{} kb/s · padded {} · sync gaps {} ({} bytes)",
            opt(&m.mpeg_version), "", opt(&m.layer), opt(&m.sample_rate), opt(&m.channel_mode), m.frames,
            if m.frames_capped { " (capped)" } else { "" }, if m.vbr { "VBR" } else { "CBR" },
            opt(&m.min_bitrate_kbps), opt(&m.max_bitrate_kbps), m.padded_frames, m.sync_gaps, m.skipped_bytes)}));
        if let Some(x) = &m.xing {
            overview.push(json!({"label": "vbr_header", "value": format!("{} · frames {} · bytes {} · encoder {}", x.tag, opt(&x.frames), opt(&x.bytes), opt(&x.encoder))}));
        }
    }
    let streams: Vec<Value> = info
        .streams
        .iter()
        .map(|s| {
            let params = [
                s.sample_rate.map(|x| format!("{} Hz", x)),
                s.channels.map(|x| format!("{} ch", x)),
                s.bits_per_sample.map(|x| format!("{} bit", x)),
                s.width.zip(s.height).map(|(w, h)| format!("{}x{}", w, h)),
                s.bitrate_kbps.map(|x| format!("{} kb/s", x)),
                s.language.clone(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            json!({"index": s.index.to_string(), "kind": s.kind, "codec": s.codec, "params": params,
                   "timescale": opt(&s.timescale), "duration_ms": opt(&s.duration_ms), "samples": opt(&s.samples),
                   "handler": s.handler_name.clone().unwrap_or_default(),
                   "compressor": s.compressor_name.clone().unwrap_or_default()})
        })
        .collect();
    let times: Vec<Value> = info
        .times
        .iter()
        .map(|t| json!({"source": t.source, "raw": t.raw, "utc": t.utc.clone().unwrap_or_else(|| "—".into()), "origin": t.origin}))
        .collect();
    let tags: Vec<Value> = info
        .tags
        .iter()
        .map(|t| json!({"source": t.source, "key": t.key, "value": t.value}))
        .collect();
    let encoders: Vec<Value> = info
        .encoders
        .iter()
        .map(|e| json!({"source": e.source, "value": e.value, "label": e.label, "category": e.category.key()}))
        .collect();
    let durations: Vec<Value> = info
        .durations
        .iter()
        .map(|d| json!({"source": d.source, "ms": d.ms.to_string(), "kind": d.kind}))
        .collect();
    let anomalies: Vec<Value> = info
        .anomalies
        .iter()
        .map(|a| json!({"kind": a.kind.key(), "offset": a.offset.to_string(), "detail": a.detail}))
        .collect();
    let tree: Vec<Value> = info
        .nodes
        .iter()
        .map(|n| {
            json!({"depth": n.depth.to_string(), "indent": "· ".repeat(n.depth as usize), "kind": n.kind,
                   "offset": n.offset.to_string(), "size": n.size.to_string(), "note": n.note})
        })
        .collect();
    json!({
        "container": info.container.key(),
        "container_name": info.container.name(),
        "overview": overview,
        "streams": capped(streams, REPORT_ROWS),
        "times": capped(times, REPORT_ROWS),
        "tags": capped(tags, REPORT_ROWS),
        "tags_truncated": info.tags_truncated,
        "encoders": capped(encoders, REPORT_ROWS),
        "device_claims": info.device_claims.iter().map(|t| format!("{} {} = {}", t.source, t.key, t.value)).collect::<Vec<_>>(),
        "durations": capped(durations, REPORT_ROWS),
        "anomalies": capped(anomalies, REPORT_ROWS),
        "anomaly_counts": info.anomaly_counts,
        "tree": capped(tree, REPORT_TREE_ROWS),
        "tree_truncated": info.nodes_truncated,
        "notes": info.notes,
    })
}

/// Streams the whole file through the run's primary algorithm (second, read-only pass).
fn rehash(path: &Path, algo: crate::hash::HashAlgo) -> std::io::Result<(String, u64)> {
    let f = std::fs::File::open(path)?;
    match crate::hash::hash_reader(f, &[algo], &mut |_| true)? {
        Some(o) => Ok((
            o.digests.first().map(|d| d.1.clone()).unwrap_or_default(),
            o.bytes,
        )),
        None => Err(std::io::Error::other("hashing stopped")),
    }
}

fn parse_file(path: &Path) -> std::io::Result<(MediaInfo, u64)> {
    let f = std::fs::File::open(path)?;
    let mut src = rv_avauth_media::FileSource::new(f)?;
    let info = rv_avauth_media::parse(&mut src);
    Ok((info, src.bytes_read))
}

/// Pipeline of one media item: re-hash → parse → signals → reference comparison → outputs.
pub fn analyze_media(ctx: &AnalyzeCtx) -> Analysis {
    let base = analyze(ctx.file_name, ctx.size, ctx.head);
    let mut findings = base.findings.clone();
    let mut details = json!({});
    if let Some((algo, first)) = ctx.expected_digest {
        match rehash(ctx.path, algo) {
            Ok((second, bytes)) => {
                let same = second.eq_ignore_ascii_case(first);
                if !same {
                    findings.push(
                        Finding::new("EVIDENCE_CHANGED_DURING_ANALYSIS", Severity::High)
                            .with("algorithm", algo.name())
                            .with("first_pass", first)
                            .with("second_pass", second.clone()),
                    );
                }
                details["evidence_rehash"] = json!({"algorithm": algo.name(), "first_pass": first,
                    "second_pass": second, "bytes": bytes, "match": same});
            }
            Err(e) => {
                findings.push(
                    Finding::new("PARSER_FAILED", Severity::High)
                        .with("reason", "unreadable")
                        .with("exit_code", "-"),
                );
                details["media_analysis"] = json!("not_completed");
                details["error"] = json!(format!("cannot read the file: {}", e));
                return Analysis {
                    findings,
                    details,
                    ..base
                };
            }
        }
    }
    let (info, bytes_read) = match parse_file(ctx.path) {
        Ok(x) => x,
        Err(e) => {
            findings.push(
                Finding::new("PARSER_FAILED", Severity::High)
                    .with("reason", "unreadable")
                    .with("exit_code", "-"),
            );
            details["media_analysis"] = json!("not_completed");
            details["error"] = json!(format!("cannot read the file: {}", e));
            return Analysis {
                findings,
                details,
                ..base
            };
        }
    };
    let sctx = SignalCtx {
        extension: base.extension.clone(),
        fs_modified: ctx.fs_modified,
        now: now_unix(),
    };
    let signals = rv_avauth_media::signals::signals(&info, &sctx);
    let mut w = Writer {
        dir: ctx.item_dir,
        outputs: vec![],
        errors: vec![],
    };
    details["container"] = json!(info.container.key());
    details["options"] = json!({"references": ctx.opts.references.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()});
    if info.container == Container::Unknown {
        // Named like media, content not recognised: keep the inventory findings (EXTENSION_MISMATCH says what
        // it really is) and say that no container was found.
        details["media_analysis"] = json!("not_recognised");
        findings.extend(signals.iter().map(finding));
        return Analysis {
            findings,
            details,
            ..base
        };
    }
    // The container check replaces the generic signature check for media files.
    findings.retain(|f| f.code != "EXTENSION_MISMATCH");
    findings.extend(signals.iter().map(finding));
    details["media_analysis"] = json!("completed");
    details["bytes_parsed_read"] = json!(bytes_read);
    details["media"] = media_summary(&info);
    w.write(
        "structure.json",
        "structure",
        &crate::report::pretty(&json!(info)),
    );
    w.write(
        "metadata.json",
        "metadata",
        &crate::report::pretty(
            &json!({"tags": info.tags, "tags_truncated": info.tags_truncated,
            "encoders": info.encoders, "device_claims": info.device_claims, "times": info.times}),
        ),
    );
    if !ctx.opts.references.is_empty() {
        let fp = rv_avauth_media::reference::fingerprint(&info);
        let mut refs = vec![];
        let mut tables = vec![];
        for r in &ctx.opts.references {
            let name = r
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let sha = rehash(r, crate::hash::HashAlgo::Sha256).ok().map(|x| x.0);
            match parse_file(r) {
                Ok((ri, _)) if ri.container != Container::Unknown => {
                    let rows = rv_avauth_media::reference::compare(
                        &fp,
                        &rv_avauth_media::reference::fingerprint(&ri),
                    );
                    findings.push(finding(&rv_avauth_media::reference::signal(&name, &rows)));
                    refs.push(json!({"path": r.display().to_string(), "sha256": sha, "status": "parsed", "container": ri.container.key()}));
                    tables.push(json!({"reference": name, "rows": rows}));
                }
                Ok(_) => {
                    findings.push(
                        Finding::new("REFERENCE_UNREADABLE", Severity::Low)
                            .with("reference", name.clone())
                            .with("reason", "no supported container"),
                    );
                    refs.push(json!({"path": r.display().to_string(), "sha256": sha, "status": "not_recognised"}));
                }
                Err(e) => {
                    findings.push(
                        Finding::new("REFERENCE_UNREADABLE", Severity::Low)
                            .with("reference", name.clone())
                            .with("reason", e.to_string()),
                    );
                    refs.push(json!({"path": r.display().to_string(), "status": "error", "error": e.to_string()}));
                }
            }
        }
        details["references"] = json!(refs);
        if !tables.is_empty() {
            details["reference_tables"] = json!(tables);
            w.write(
                "reference-comparison.json",
                "reference_comparison",
                &crate::report::pretty(&json!({"evidence": fp, "comparisons": tables})),
            );
        }
    }
    if !w.errors.is_empty() {
        details["output_errors"] = json!(w.errors);
    }
    Analysis {
        detected_type: Some(info.container.name().to_string()),
        extension: base.extension,
        findings,
        details,
        outputs: w.outputs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::DomainOptions;

    fn sample(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/sample")
            .join(name)
    }

    fn run(path: &Path, dir: &Path, opts: &DomainOptions) -> Analysis {
        let bytes = std::fs::read(path).unwrap();
        let head = &bytes[..bytes.len().min(4096)];
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let digest = crate::hash::hash_bytes(&bytes, crate::hash::HashAlgo::Sha256);
        analyze_media(&AnalyzeCtx {
            path,
            file_name: &name,
            size: bytes.len() as u64,
            head,
            item_dir: dir,
            opts,
            expected_digest: Some((crate::hash::HashAlgo::Sha256, &digest)),
            fs_modified: None,
        })
    }

    #[test]
    fn candidates() {
        assert!(is_media_candidate("a.bin", b"RIFF\0\0\0\0WAVE"));
        assert!(is_media_candidate("damaged.MP4", b"garbage"));
        assert!(!is_media_candidate("notes.txt", b"hello"));
    }

    #[test]
    fn a_wave_file_is_parsed_rehashed_and_its_outputs_listed() {
        let dir = tempfile::tempdir().unwrap();
        let a = run(
            &sample("evidence/tone-original.wav"),
            dir.path(),
            &DomainOptions::default(),
        );
        assert_eq!(a.details["media_analysis"], "completed");
        assert_eq!(a.details["evidence_rehash"]["match"], true);
        assert_eq!(a.detected_type.as_deref(), Some("RIFF/WAVE"));
        let kinds: Vec<&str> = a.outputs.iter().map(|o| o.kind.as_str()).collect();
        assert_eq!(kinds, ["structure", "metadata"]);
        assert!(dir.path().join("structure.json").is_file());
        assert!(a.findings.iter().any(|f| f.code == "DEVICE_CLAIM"));
        let w = Writer {
            dir: dir.path(),
            outputs: vec![],
            errors: vec![],
        };
        drop(w);
    }

    #[test]
    fn reference_files_are_compared_and_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let opts = DomainOptions {
            references: vec![
                sample("reference/camera-reference.mov"),
                sample("evidence/notes.txt"),
                sample("reference/missing.mov"),
            ],
            ..DomainOptions::default()
        };
        let a = run(&sample("evidence/camera-original.mov"), dir.path(), &opts);
        let codes: Vec<&str> = a.findings.iter().map(|f| f.code.as_str()).collect();
        assert!(codes.contains(&"REFERENCE_CONSISTENT"), "{codes:?}");
        assert_eq!(
            codes
                .iter()
                .filter(|c| **c == "REFERENCE_UNREADABLE")
                .count(),
            2
        );
        assert_eq!(a.details["references"][0]["status"], "parsed");
        assert_eq!(
            a.details["references"][0]["sha256"].as_str().unwrap().len(),
            64
        );
        assert!(dir.path().join("reference-comparison.json").is_file());
        let dir2 = tempfile::tempdir().unwrap();
        let b = run(&sample("evidence/voice-reencoded.m4a"), dir2.path(), &opts);
        assert!(b.findings.iter().any(|f| f.code == "REFERENCE_DIFFERS"));
    }

    #[test]
    fn content_that_is_not_media_keeps_the_inventory_findings() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("clip.mp4");
        std::fs::write(&fake, b"%PDF-1.7\n%%EOF\n").unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let a = run(&fake, &out, &DomainOptions::default());
        let codes: Vec<&str> = a.findings.iter().map(|f| f.code.as_str()).collect();
        assert_eq!(codes, ["EXTENSION_MISMATCH", "MEDIA_NOT_RECOGNISED"]);
        assert_eq!(a.details["media_analysis"], "not_recognised");
        assert!(a.outputs.is_empty());
    }

    /// Every code created with `Finding::new` / `Signal::new` in the core and the media crate is worded
    /// (title, shows, does not prove) in the three catalogues.
    #[test]
    fn every_finding_code_is_worded_in_every_language() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut codes = std::collections::BTreeSet::new();
        for dir in ["src", "src/report", "../media/src"] {
            for e in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap();
                let text = text.split("#[cfg(test)]").next().unwrap_or_default();
                for part in text.split("::new(\"").skip(1) {
                    let code: String = part.chars().take_while(|c| *c != '"').collect();
                    if !code.is_empty() && code.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                    {
                        codes.insert(code);
                    }
                }
                // Signals built from a table: (kind, "CODE", "severity").
                for part in text.split(", \"").skip(1) {
                    let code: String = part.chars().take_while(|c| *c != '"').collect();
                    if code.len() > 6
                        && code.contains('_')
                        && code.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                    {
                        codes.insert(code);
                    }
                }
            }
        }
        assert!(codes.contains("OGG_CRC_MISMATCH") && codes.contains("DURATION_MISMATCH"));
        for lang in ["en", "es", "fr"] {
            let c = crate::i18n::catalog(lang);
            for code in &codes {
                let (title, shows, dnp) = c.finding(code, &Default::default());
                assert!(
                    title != *code && shows != *code && dnp != *code,
                    "{lang}: {code} is not worded"
                );
            }
        }
    }
}
