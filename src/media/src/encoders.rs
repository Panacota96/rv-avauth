//! Classifies encoder / writing-software strings (case-insensitive substring match, first rule wins).
//!
//! The table is deliberately small and conservative: it names software families whose strings are
//! documented by their publishers or visible in their own output. A match says which software wrote the
//! string, not who used it or why. Strings that match nothing stay `unknown` and are still reported.

use crate::model::{EncoderCategory as C, MediaInfo};

/// (pattern, label, category). Patterns are lowercase.
pub const RULES: &[(&str, &str, C)] = &[
    // Online converters first: they often embed an FFmpeg string too.
    (
        "online-audio-converter",
        "online-audio-converter.com",
        C::OnlineConverter,
    ),
    ("online-convert", "online-convert.com", C::OnlineConverter),
    ("convertio", "Convertio", C::OnlineConverter),
    ("cloudconvert", "CloudConvert", C::OnlineConverter),
    ("zamzar", "Zamzar", C::OnlineConverter),
    ("123apps", "123apps", C::OnlineConverter),
    ("freeconvert", "FreeConvert", C::OnlineConverter),
    ("media.io", "Media.io", C::OnlineConverter),
    // Editors.
    ("audacity", "Audacity", C::Editor),
    (
        "adobe",
        "Adobe (Audition / Premiere / Media Encoder)",
        C::Editor,
    ),
    ("audition", "Adobe Audition", C::Editor),
    ("premiere", "Adobe Premiere", C::Editor),
    ("imovie", "Apple iMovie", C::Editor),
    ("final cut", "Apple Final Cut", C::Editor),
    ("garageband", "Apple GarageBand", C::Editor),
    ("logic pro", "Apple Logic Pro", C::Editor),
    ("davinci", "DaVinci Resolve", C::Editor),
    ("capcut", "CapCut", C::Editor),
    ("kinemaster", "KineMaster", C::Editor),
    ("inshot", "InShot", C::Editor),
    ("vegas", "VEGAS", C::Editor),
    ("camtasia", "Camtasia", C::Editor),
    ("shotcut", "Shotcut", C::Editor),
    ("openshot", "OpenShot", C::Editor),
    ("filmora", "Wondershare Filmora", C::Editor),
    ("reaper", "REAPER", C::Editor),
    ("sound forge", "Sound Forge", C::Editor),
    ("wavelab", "WaveLab", C::Editor),
    ("ocenaudio", "ocenaudio", C::Editor),
    ("goldwave", "GoldWave", C::Editor),
    ("wavepad", "WavePad", C::Editor),
    // Transcoders / multiplexers.
    ("lavf", "FFmpeg (libavformat)", C::Transcoder),
    ("lavc", "FFmpeg (libavcodec)", C::Transcoder),
    ("ffmpeg", "FFmpeg", C::Transcoder),
    ("libav", "Libav / FFmpeg", C::Transcoder),
    ("handbrake", "HandBrake", C::Transcoder),
    ("gpac", "GPAC / MP4Box", C::Transcoder),
    ("mp4box", "GPAC / MP4Box", C::Transcoder),
    ("mkvmerge", "MKVToolNix", C::Transcoder),
    ("sox", "SoX", C::Transcoder),
    // Messengers.
    ("whatsapp", "WhatsApp", C::Messenger),
    ("telegram", "Telegram", C::Messenger),
    // Encoder libraries also used by ordinary recorders.
    ("lame", "LAME MP3 encoder", C::EncoderLibrary),
    ("libvorbis", "libvorbis (Xiph.Org)", C::EncoderLibrary),
    ("xiph.org", "Xiph.Org reference encoder", C::EncoderLibrary),
    ("libopus", "libopus", C::EncoderLibrary),
    ("opus-tools", "opus-tools", C::EncoderLibrary),
    ("fraunhofer", "Fraunhofer encoder", C::EncoderLibrary),
    ("itunes", "Apple iTunes / Music", C::EncoderLibrary),
    // Devices and recorder apps.
    ("voice memos", "Apple Voice Memos", C::RecorderApp),
    ("recorder", "recorder app or device", C::RecorderApp),
    ("zoom h", "Zoom handy recorder", C::Device),
    ("tascam", "TASCAM recorder", C::Device),
    ("olympus", "Olympus recorder", C::Device),
    ("sony", "Sony device", C::Device),
    ("samsung", "Samsung device", C::Device),
    ("android", "Android", C::Device),
    ("iphone", "Apple iPhone", C::Device),
    ("apple", "Apple software", C::Device),
];

/// Handler names that identify the multiplexer (exact match). Observed in the output of the named
/// software; to be verified against device originals before relying on them (see docs/theory.md).
pub const HANDLER_NAMES: &[(&str, &str, C)] = &[
    (
        "SoundHandler",
        "FFmpeg (libavformat) default handler name",
        C::Transcoder,
    ),
    (
        "VideoHandler",
        "FFmpeg (libavformat) default handler name",
        C::Transcoder,
    ),
    ("SoundHandle", "Android MPEG4Writer handler name", C::Device),
    ("VideoHandle", "Android MPEG4Writer handler name", C::Device),
    ("Core Media Audio", "Apple Core Media", C::Device),
    ("Core Media Video", "Apple Core Media", C::Device),
    ("Core Media Metadata", "Apple Core Media", C::Device),
    ("GPAC ISO Audio Handler", "GPAC / MP4Box", C::Transcoder),
    ("GPAC ISO Video Handler", "GPAC / MP4Box", C::Transcoder),
];

/// Sample-entry vendor fourccs.
pub const VENDORS: &[(&str, &str, C)] = &[
    ("appl", "Apple", C::Device),
    ("FFMP", "FFmpeg (libavformat)", C::Transcoder),
];

pub fn classify(source: &str, value: &str) -> (String, C) {
    if source.contains(" name") && source.starts_with("hdlr") {
        return HANDLER_NAMES
            .iter()
            .find(|(n, _, _)| *n == value)
            .map(|(_, l, c)| (l.to_string(), *c))
            .unwrap_or((String::new(), C::Unknown));
    }
    if source.ends_with(" vendor") && source.starts_with("stsd") {
        return VENDORS
            .iter()
            .find(|(n, _, _)| *n == value)
            .map(|(_, l, c)| (l.to_string(), *c))
            .unwrap_or((String::new(), C::Unknown));
    }
    let k = source.to_ascii_lowercase();
    if k.contains("com.apple.quicktime.software") {
        return ("Apple device software (iOS / macOS)".into(), C::Device);
    }
    if k.contains("com.android.version") {
        return ("Android OS version".into(), C::Device);
    }
    let v = value.to_ascii_lowercase();
    RULES
        .iter()
        .find(|(p, _, _)| v.contains(p))
        .map(|(_, l, c)| (l.to_string(), *c))
        .unwrap_or((String::new(), C::Unknown))
}

/// Sets label and category of every trace; drops handler names that match nothing (a handler name is
/// only an encoder trace when it is a known default).
pub fn classify_all(info: &mut MediaInfo) {
    for e in &mut info.encoders {
        let (l, c) = classify(&e.source, &e.value);
        e.label = l;
        e.category = c;
    }
    info.encoders
        .retain(|e| !(e.source.starts_with("hdlr") && e.category == C::Unknown));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify("ilst ©too", "Lavf60.16.100").1, C::Transcoder);
        assert_eq!(classify("INFO ISFT", "Audacity 3.4").1, C::Editor);
        assert_eq!(
            classify("Xing tag encoder", "LAME3.100").1,
            C::EncoderLibrary
        );
        assert_eq!(
            classify("OpusTags vendor", "libopus 1.4").1,
            C::EncoderLibrary
        );
        assert_eq!(
            classify("keys com.apple.quicktime.software", "17.1").1,
            C::Device
        );
        assert_eq!(classify("hdlr[1] name", "SoundHandler").1, C::Transcoder);
        assert_eq!(classify("hdlr[1] name", "Mono").1, C::Unknown);
        assert_eq!(
            classify("ID3v2.3 TSSE", "online-audio-converter.com (Lavf)").1,
            C::OnlineConverter
        );
        assert_eq!(classify("stsd[1] vendor", "appl").1, C::Device);
        assert_eq!(classify("x", "something else").1, C::Unknown);
    }
}
