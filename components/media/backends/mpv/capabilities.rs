/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! What the linked libmpv can actually play, used to answer `canPlayType()`.
//!
//! Support is never hardcoded: the decoders and demuxers are read from libmpv once, and
//! the tables below only translate web names (MIME types, RFC 6381 codec strings) into
//! FFmpeg names.

use std::collections::HashSet;

use data_url::mime::Mime;
use servo_media::SupportsMediaType;

use crate::handle::{MpvError, MpvHandle, Node};

/// A web container MIME type, the FFmpeg demuxer able to read it, and the decoder needed
/// when the container can only hold one codec.
struct Container {
    mime_type: &'static str,
    demuxer: &'static str,
    implied_decoder: Option<&'static str>,
}

const CONTAINERS: &[Container] = &[
    container("audio/mpeg", "mp3", Some("mp3")),
    container("audio/mp3", "mp3", Some("mp3")),
    container("audio/aac", "aac", Some("aac")),
    container("audio/flac", "flac", Some("flac")),
    container("audio/mp4", "mp4", None),
    container("audio/x-m4a", "mp4", None),
    container("video/mp4", "mp4", None),
    container("audio/webm", "webm", None),
    container("video/webm", "webm", None),
    container("audio/ogg", "ogg", None),
    container("video/ogg", "ogg", None),
    container("application/ogg", "ogg", None),
    container("audio/wav", "wav", None),
    container("audio/vnd.wave", "wav", None),
    container("audio/wave", "wav", None),
    container("audio/x-wav", "wav", None),
];

const fn container(
    mime_type: &'static str,
    demuxer: &'static str,
    implied_decoder: Option<&'static str>,
) -> Container {
    Container {
        mime_type,
        demuxer,
        implied_decoder,
    }
}

/// RFC 6381 codec strings (lowercase, a trailing `*` is a prefix match) to FFmpeg codec names.
/// The first matching entry wins, so specific strings must come before prefixes.
const CODECS: &[(&str, &str)] = &[
    ("avc1*", "h264"),
    ("avc3*", "h264"),
    ("hev1*", "hevc"),
    ("hvc1*", "hevc"),
    ("vp8*", "vp8"),
    ("vp9*", "vp9"),
    ("vp09*", "vp9"),
    ("av01*", "av1"),
    // MPEG-4 audio object type 34 is MPEG-1 Layer III, every other mp4a.40.x is AAC.
    ("mp4a.40.34", "mp3"),
    ("mp4a.40*", "aac"),
    // Object type indications for MPEG-2 and MPEG-1 audio (mp4ra.org).
    ("mp4a.69", "mp3"),
    ("mp4a.6b", "mp3"),
    ("mp3", "mp3"),
    ("opus", "opus"),
    ("vorbis", "vorbis"),
    ("flac", "flac"),
    ("theora", "theora"),
    // WAVE_FORMAT_PCM (RFC 2361). FFmpeg always builds its PCM decoders.
    ("1", "pcm_s16le"),
];

/// Options for the short-lived instance used to read the capabilities: it never plays
/// anything, so no output is ever opened.
const PROBE_OPTIONS: &[(&str, &str)] = &[("ao", "null"), ("vid", "no")];

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Capabilities {
    /// Canonical FFmpeg codec names that have at least one decoder.
    decoders: HashSet<String>,
    /// FFmpeg demuxer names.
    demuxers: HashSet<String>,
}

impl Capabilities {
    /// Reads the capabilities from a temporary libmpv instance, destroyed before returning.
    pub fn probe() -> Result<Capabilities, MpvError> {
        let options: Vec<_> = crate::BASE_OPTIONS
            .iter()
            .chain(PROBE_OPTIONS)
            .copied()
            .collect();
        let handle = MpvHandle::new(&options)?;
        Capabilities::from_nodes(
            &handle.get_node("decoder-list")?,
            &handle.get_node("demuxer-lavf-list")?,
        )
    }

    /// Builds the capabilities from the `decoder-list` and `demuxer-lavf-list` properties.
    fn from_nodes(decoder_list: &Node, demuxer_list: &Node) -> Result<Capabilities, MpvError> {
        let Node::Array(decoder_entries) = decoder_list else {
            return Err(unexpected("decoder-list"));
        };
        let Node::Array(demuxer_entries) = demuxer_list else {
            return Err(unexpected("demuxer-lavf-list"));
        };

        let decoders = decoder_entries
            .iter()
            .filter_map(|entry| entry.get("codec")?.as_str())
            .map(str::to_owned)
            .collect();
        // libavformat names demuxers after every format they handle, e.g. "mov,mp4,m4a".
        let demuxers = demuxer_entries
            .iter()
            .filter_map(Node::as_str)
            .flat_map(|names| names.split(','))
            .map(str::to_owned)
            .collect();

        Ok(Capabilities { decoders, demuxers })
    }

    /// Implements `HTMLMediaElement.canPlayType()` semantics, as the GStreamer backend does:
    /// - unknown container or missing demuxer: `No`;
    /// - no `codecs` parameter: `Maybe`;
    /// - every listed codec has a decoder: `Probably`, otherwise `No`.
    pub fn can_play_type(&self, media_type: &str) -> SupportsMediaType {
        let Ok(mime) = media_type.parse::<Mime>() else {
            return SupportsMediaType::No;
        };
        let mime_type = format!("{}/{}", mime.type_, mime.subtype).to_ascii_lowercase();
        let Some(container) = CONTAINERS
            .iter()
            .find(|container| container.mime_type == mime_type)
        else {
            return SupportsMediaType::No;
        };
        if !self.demuxers.contains(container.demuxer) {
            return SupportsMediaType::No;
        }
        if let Some(decoder) = container.implied_decoder &&
            !self.decoders.contains(decoder)
        {
            return SupportsMediaType::No;
        }

        let Some(codecs) = mime.get_parameter("codecs") else {
            return SupportsMediaType::Maybe;
        };
        let all_supported = codecs
            .split(',')
            .map(|codec| codec.trim().to_ascii_lowercase())
            .all(|codec| {
                ffmpeg_codec(&codec).is_some_and(|decoder| self.decoders.contains(decoder))
            });
        if all_supported {
            SupportsMediaType::Probably
        } else {
            SupportsMediaType::No
        }
    }
}

fn ffmpeg_codec(codec: &str) -> Option<&'static str> {
    CODECS
        .iter()
        .find(|(pattern, _)| match pattern.strip_suffix('*') {
            Some(prefix) => codec.starts_with(prefix),
            None => codec == *pattern,
        })
        .map(|(_, ffmpeg_name)| *ffmpeg_name)
}

fn unexpected(property: &str) -> MpvError {
    MpvError::UnexpectedFormat {
        property: property.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities(decoders: &[&str], demuxers: &[&str]) -> Capabilities {
        let decoder_list = Node::Array(
            decoders
                .iter()
                .map(|codec| {
                    Node::Map(vec![
                        ("codec".to_owned(), Node::String((*codec).to_owned())),
                        ("driver".to_owned(), Node::String((*codec).to_owned())),
                    ])
                })
                .collect(),
        );
        let demuxer_list = Node::Array(
            demuxers
                .iter()
                .map(|names| Node::String((*names).to_owned()))
                .collect(),
        );
        Capabilities::from_nodes(&decoder_list, &demuxer_list).unwrap()
    }

    fn typical() -> Capabilities {
        capabilities(
            &["aac", "mp3", "opus", "vorbis", "h264", "vp9"],
            &["mov,mp4,m4a,3gp,3g2,mj2", "matroska,webm", "mp3", "ogg"],
        )
    }

    #[test]
    fn container_without_codecs_is_maybe() {
        assert_eq!(
            typical().can_play_type("audio/mp4"),
            SupportsMediaType::Maybe
        );
        assert_eq!(
            typical().can_play_type("video/webm"),
            SupportsMediaType::Maybe
        );
    }

    #[test]
    fn supported_codecs_are_probably() {
        let capabilities = typical();
        assert_eq!(
            capabilities.can_play_type("audio/mp4; codecs=\"mp4a.40.2\""),
            SupportsMediaType::Probably
        );
        assert_eq!(
            capabilities.can_play_type("video/mp4; codecs=\"avc1.42E01E, mp4a.40.2\""),
            SupportsMediaType::Probably
        );
        assert_eq!(
            capabilities.can_play_type("video/webm; codecs=\"vp09.00.10.08, opus\""),
            SupportsMediaType::Probably
        );
    }

    #[test]
    fn one_unsupported_codec_is_no() {
        assert_eq!(
            typical().can_play_type("video/mp4; codecs=\"av01.0.05M.08, mp4a.40.2\""),
            SupportsMediaType::No
        );
        assert_eq!(
            typical().can_play_type("audio/ogg; codecs=\"unknown\""),
            SupportsMediaType::No
        );
    }

    #[test]
    fn missing_demuxer_or_implied_decoder_is_no() {
        assert_eq!(typical().can_play_type("audio/wav"), SupportsMediaType::No);
        let without_mp3 = capabilities(&["aac"], &["mp3"]);
        assert_eq!(
            without_mp3.can_play_type("audio/mpeg"),
            SupportsMediaType::No
        );
    }

    #[test]
    fn unknown_or_malformed_types_are_no() {
        assert_eq!(
            typical().can_play_type("video/x-nope"),
            SupportsMediaType::No
        );
        assert_eq!(
            typical().can_play_type("not a mime type"),
            SupportsMediaType::No
        );
        assert_eq!(typical().can_play_type(""), SupportsMediaType::No);
    }

    #[test]
    fn mp3_in_mp4_is_not_mistaken_for_aac() {
        let mp3_only = capabilities(&["mp3"], &["mov,mp4,m4a,3gp,3g2,mj2"]);
        assert_eq!(
            mp3_only.can_play_type("audio/mp4; codecs=\"mp4a.40.34\""),
            SupportsMediaType::Probably
        );
        assert_eq!(
            mp3_only.can_play_type("audio/mp4; codecs=\"mp4a.40.2\""),
            SupportsMediaType::No
        );
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(
            typical().can_play_type("AUDIO/MP4; codecs=\"MP4A.40.2\""),
            SupportsMediaType::Probably
        );
    }

    #[test]
    fn unexpected_property_shapes_are_errors() {
        let error = Capabilities::from_nodes(&Node::None, &Node::Array(vec![])).unwrap_err();
        assert_eq!(error, unexpected("decoder-list"));
    }
}
