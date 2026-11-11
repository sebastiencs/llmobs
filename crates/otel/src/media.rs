//! Media extraction from payloads.
//!
//! Spec 02 § Media extraction. Runs after message conversion and before
//! truncation. A blob under `media_min_bytes`, or base64 that is not the
//! standard alphabet, stays inline. A blob over `media_max_bytes` is replaced
//! and never decoded.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use base64::alphabet::STANDARD as STANDARD_ALPHABET;
use base64::engine::general_purpose::{GeneralPurpose, PAD_INDIFFERENT};
use llmobs_core::media::MediaBlob;
use llmobs_core::message::{Message, Modality, Part};
use llmobs_core::names::media::{DROPPED_URI, URI_SCHEME};
use serde_json::{Map, Value as Json};
use sha2::{Digest, Sha256};

use crate::normalize::IngestLimits;
use crate::payload::{Io, Payload};

/// Standard alphabet, padding optional. Whitespace is rejected.
const DECODER: GeneralPurpose = GeneralPurpose::new(&STANDARD_ALPHABET, PAD_INDIFFERENT);

const OCTET_STREAM: &str = "application/octet-stream";

/// What replaced one candidate, when it was not left inline.
enum Replacement {
    /// `media://<sha256>` plus the MIME type stored for that blob.
    Extracted {
        uri: String,
        mime: String,
        modality: Option<Modality>,
    },
    /// Over `media_max_bytes`. `size` is the decoded length estimated from the
    /// base64 length, before any decode.
    Dropped { size: usize },
}

/// Request-wide blobs and the current span's references.
pub(crate) struct MediaSink<'a> {
    limits: &'a IngestLimits,
    blobs: BTreeMap<String, MediaBlob>,
    refs: BTreeSet<String>,
}

impl<'a> MediaSink<'a> {
    pub(crate) fn new(limits: &'a IngestLimits) -> Self {
        Self {
            limits,
            blobs: BTreeMap::new(),
            refs: BTreeSet::new(),
        }
    }

    pub(crate) fn clear_refs(&mut self) {
        self.refs.clear();
    }

    /// The current span's hashes, sorted and unique.
    pub(crate) fn media_refs(&self) -> Vec<String> {
        self.refs.iter().cloned().collect()
    }

    /// Every blob in the request, sorted by SHA-256.
    pub(crate) fn into_media(self) -> Vec<MediaBlob> {
        self.blobs.into_values().collect()
    }

    /// Replace one base64 candidate.
    ///
    /// `None` leaves the source inline: the decoded size is under
    /// `media_min_bytes`, or the text is not standard base64. The size is
    /// `len * 3 / 4` minus padding, and a value over `media_max_bytes` returns
    /// [`Replacement::Dropped`] without decoding.
    fn extract(&mut self, base64: &str, declared_mime: Option<&str>) -> Option<Replacement> {
        let size = decoded_len(base64)?;
        if size < self.limits.media_min_bytes {
            return None;
        }
        if size > self.limits.media_max_bytes {
            return Some(Replacement::Dropped { size });
        }
        let bytes = DECODER.decode(base64).ok()?;
        if bytes.len() > self.limits.media_max_bytes {
            return Some(Replacement::Dropped { size: bytes.len() });
        }
        if bytes.len() < self.limits.media_min_bytes {
            return None;
        }
        let sha = hex_sha256(&bytes);
        let mime = if let Some(existing) = self.blobs.get(&sha) {
            existing.mime_type.clone()
        } else {
            let mime = resolve_mime(declared_mime, &bytes);
            self.blobs.insert(
                sha.clone(),
                MediaBlob {
                    sha256: sha.clone(),
                    mime_type: mime.clone(),
                    bytes,
                },
            );
            mime
        };
        self.refs.insert(sha.clone());
        Some(Replacement::Extracted {
            uri: format!("{URI_SCHEME}{sha}"),
            modality: modality_of(&mime),
            mime,
        })
    }
}

/// Messages, JSON, and text. Spec 02 § Media extraction.
pub(crate) fn extract_payload(payload: &mut Payload, sink: &mut MediaSink<'_>) {
    match payload {
        Payload::Messages(messages) => {
            for message in messages {
                extract_message(message, sink);
            }
        }
        Payload::Json(value) => extract_json(value, sink),
        Payload::Text(text) => replace_text(text, sink),
    }
}

/// `system_instructions`, tool definitions, and JSON inside messages.
pub(crate) fn extract_json(value: &mut Json, sink: &mut MediaSink<'_>) {
    match value {
        Json::Array(items) => {
            for item in items {
                extract_json(item, sink);
            }
        }
        Json::String(text) => replace_text(text, sink),
        Json::Object(map) => {
            if let Some(replaced) = json_part(map, sink) {
                *value = replaced;
                return;
            }
            replace_provider_shapes(map, sink);
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(child) = map.get_mut(&key) {
                    extract_json(child, sink);
                }
            }
        }
        Json::Null | Json::Bool(_) | Json::Number(_) => {}
    }
}

pub(crate) fn extract_io(io: &mut Io, sink: &mut MediaSink<'_>) {
    if let Some(payload) = io.input.as_mut() {
        extract_payload(payload, sink);
    }
    if let Some(payload) = io.output.as_mut() {
        extract_payload(payload, sink);
    }
    if let Some(value) = io.system_instructions.as_mut() {
        extract_json(value, sink);
    }
    if let Some(items) = io.tool_definitions.as_mut() {
        for item in items {
            extract_json(item, sink);
        }
    }
}

fn extract_message(message: &mut Message, sink: &mut MediaSink<'_>) {
    for part in &mut message.parts {
        extract_part(part, sink);
    }
    extract_map(&mut message.extra, sink);
}

fn extract_part(part: &mut Part, sink: &mut MediaSink<'_>) {
    match part {
        Part::Blob {
            content,
            mime_type,
            extra,
            ..
        } => {
            let declared = mime_type.clone();
            let Some(replacement) = sink.extract(content, declared.as_deref()) else {
                return;
            };
            *part = uri_part(std::mem::take(extra), replacement);
        }
        Part::Uri { uri, extra, .. } => {
            if let Some(data) = entire_data_uri(uri)
                && let Some(replacement) = sink.extract(data.payload, Some(data.mime))
            {
                *part = uri_part(std::mem::take(extra), replacement);
                return;
            }
            replace_text(uri, sink);
            extract_map(extra, sink);
        }
        Part::Text { content, extra } | Part::Reasoning { content, extra } => {
            replace_text(content, sink);
            extract_map(extra, sink);
        }
        Part::ToolCall {
            arguments, extra, ..
        } => {
            if let Some(value) = arguments {
                extract_json(value, sink);
            }
            extract_map(extra, sink);
        }
        Part::ToolCallResponse {
            response, extra, ..
        } => {
            extract_json(response, sink);
            extract_map(extra, sink);
        }
        Part::Unknown(value) => extract_json(value, sink),
    }
}

fn uri_part(mut extra: Map<String, Json>, replacement: Replacement) -> Part {
    match replacement {
        Replacement::Extracted {
            uri,
            mime,
            modality,
        } => Part::Uri {
            modality,
            mime_type: Some(mime),
            uri,
            extra,
        },
        Replacement::Dropped { size } => {
            extra.insert("reason".to_owned(), Json::String("too_large".to_owned()));
            extra.insert("size".to_owned(), Json::from(size));
            Part::Uri {
                modality: None,
                mime_type: None,
                uri: DROPPED_URI.to_owned(),
                extra,
            }
        }
    }
}

fn extract_map(map: &mut Map<String, Json>, sink: &mut MediaSink<'_>) {
    replace_provider_shapes(map, sink);
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        if let Some(child) = map.get_mut(&key) {
            extract_json(child, sink);
        }
    }
}

/// A canonical `blob` part, or a `uri` part whose URI is one data URI.
fn json_part(map: &Map<String, Json>, sink: &mut MediaSink<'_>) -> Option<Json> {
    let kind = map.get("type")?.as_str()?;
    let (payload, declared, drop_keys) = match kind {
        "blob" => {
            let content = map.get("content")?.as_str()?;
            (
                content.to_owned(),
                map.get("mime_type")
                    .and_then(Json::as_str)
                    .map(str::to_owned),
                &["type", "content", "modality", "mime_type"][..],
            )
        }
        "uri" => {
            let data = entire_data_uri(map.get("uri")?.as_str()?)?;
            (
                data.payload.to_owned(),
                Some(data.mime.to_owned()),
                &["type", "uri", "modality", "mime_type"][..],
            )
        }
        _ => return None,
    };
    let replacement = sink.extract(&payload, declared.as_deref())?;
    Some(replaced_object(map, replacement, drop_keys))
}

fn replaced_object(map: &Map<String, Json>, replacement: Replacement, drop_keys: &[&str]) -> Json {
    let mut out = Map::new();
    out.insert("type".to_owned(), Json::String("uri".to_owned()));
    match replacement {
        Replacement::Extracted {
            uri,
            mime,
            modality,
        } => {
            if let Some(modality) = modality {
                out.insert(
                    "modality".to_owned(),
                    Json::String(modality.as_str().to_owned()),
                );
            }
            out.insert("mime_type".to_owned(), Json::String(mime));
            out.insert("uri".to_owned(), Json::String(uri));
        }
        Replacement::Dropped { size } => {
            out.insert("uri".to_owned(), Json::String(DROPPED_URI.to_owned()));
            out.insert("reason".to_owned(), Json::String("too_large".to_owned()));
            out.insert("size".to_owned(), Json::from(size));
        }
    }
    for (key, value) in map {
        if !drop_keys.contains(&key.as_str()) {
            out.insert(key.clone(), value.clone());
        }
    }
    Json::Object(out)
}

fn replace_provider_shapes(map: &mut Map<String, Json>, sink: &mut MediaSink<'_>) {
    if map.get("type").and_then(Json::as_str) == Some("base64") {
        replace_own(map, "data", &["media_type"], sink);
    }
    if map.get("type").and_then(Json::as_str) == Some("image") {
        replace_own(map, "image", &["mimeType", "mediaType"], sink);
    }
    if map.get("type").and_then(Json::as_str) == Some("file") {
        replace_own(map, "data", &["mediaType", "mimeType"], sink);
    }
    replace_nested(map, "input_audio", "data", &[], sink);
    replace_nested(map, "inline_data", "data", &["mime_type", "mimeType"], sink);
    replace_nested(map, "inlineData", "data", &["mimeType", "mime_type"], sink);
}

fn replace_own(
    map: &mut Map<String, Json>,
    field: &str,
    mime_keys: &[&str],
    sink: &mut MediaSink<'_>,
) {
    let Some(data) = map.get(field).and_then(Json::as_str).map(str::to_owned) else {
        return;
    };
    let mime = declared_mime(map, mime_keys);
    let Some(text) = replacement_text(sink.extract(&data, mime.as_deref())) else {
        return;
    };
    map.insert(field.to_owned(), Json::String(text));
}

fn replace_nested(
    map: &mut Map<String, Json>,
    key: &str,
    field: &str,
    mime_keys: &[&str],
    sink: &mut MediaSink<'_>,
) {
    let Some(inner) = map.get(key).and_then(Json::as_object) else {
        return;
    };
    let Some(data) = inner.get(field).and_then(Json::as_str).map(str::to_owned) else {
        return;
    };
    let mime = declared_mime(inner, mime_keys);
    let Some(text) = replacement_text(sink.extract(&data, mime.as_deref())) else {
        return;
    };
    let Some(Json::Object(inner)) = map.get_mut(key) else {
        return;
    };
    inner.insert(field.to_owned(), Json::String(text));
}

fn replacement_text(replacement: Option<Replacement>) -> Option<String> {
    match replacement? {
        Replacement::Extracted { uri, .. } => Some(uri),
        Replacement::Dropped { .. } => Some(DROPPED_URI.to_owned()),
    }
}

fn declared_mime(map: &Map<String, Json>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let text = map.get(*key)?.as_str()?;
        is_mime(text).then(|| text.to_owned())
    })
}

fn replace_text(text: &mut String, sink: &mut MediaSink<'_>) {
    if let Some(next) = replace_data_uris(text, sink) {
        *text = next;
    }
}

struct DataUri<'a> {
    mime: &'a str,
    payload: &'a str,
    len: usize,
}

fn entire_data_uri(text: &str) -> Option<DataUri<'_>> {
    let data = match_data_uri(text)?;
    if data.len == text.len() {
        Some(data)
    } else {
        None
    }
}

fn replace_data_uris(text: &str, sink: &mut MediaSink<'_>) -> Option<String> {
    if !text.contains("data:") {
        return None;
    }
    let mut out = String::new();
    let mut cursor = 0;
    let mut changed = false;
    while let Some(offset) = text[cursor..].find("data:") {
        let start = cursor + offset;
        out.push_str(&text[cursor..start]);
        if let Some(data) = match_data_uri(&text[start..])
            && let Some(replacement) = replacement_text(sink.extract(data.payload, Some(data.mime)))
        {
            out.push_str(&replacement);
            changed = true;
            cursor = start + data.len;
            continue;
        }
        out.push_str("data:");
        cursor = start + "data:".len();
    }
    if !changed {
        return None;
    }
    out.push_str(&text[cursor..]);
    Some(out)
}

fn match_data_uri(text: &str) -> Option<DataUri<'_>> {
    let rest = text.strip_prefix("data:")?;
    let (mime, after) = rest.split_once(";base64,")?;
    let bytes = after.as_bytes();
    let mut end = 0;
    while end < bytes.len() && is_b64(bytes[end]) {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'=' {
        end += 1;
        if end < bytes.len() && bytes[end] == b'=' {
            end += 1;
        }
    }
    if end == 0 {
        return None;
    }
    Some(DataUri {
        mime,
        payload: &after[..end],
        len: "data:".len() + mime.len() + ";base64,".len() + end,
    })
}

/// Decoded byte length, or `None` when `text` is not standard base64.
fn decoded_len(text: &str) -> Option<usize> {
    if text.is_empty() {
        return None;
    }
    let bytes = text.as_bytes();
    let mut padding = 0;
    let mut body = bytes.len();
    while body > 0 && bytes[body - 1] == b'=' {
        padding += 1;
        body -= 1;
        if padding > 2 {
            return None;
        }
    }
    if padding > 0 && !bytes.len().is_multiple_of(4) {
        return None;
    }
    if bytes[..body].iter().any(|byte| !is_b64(*byte)) {
        return None;
    }
    if body % 4 == 1 {
        return None;
    }
    Some(body * 3 / 4)
}

fn is_b64(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/'
}

fn is_mime(value: &str) -> bool {
    let Some((top, sub)) = value.split_once('/') else {
        return false;
    };
    is_token(top) && is_token(sub)
}

fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_'))
}

fn resolve_mime(declared: Option<&str>, bytes: &[u8]) -> String {
    if let Some(mime) = declared.filter(|mime| is_mime(mime)) {
        return mime.to_owned();
    }
    sniff(bytes).unwrap_or(OCTET_STREAM).to_owned()
}

fn modality_of(mime: &str) -> Option<Modality> {
    match mime.split_once('/')?.0 {
        "image" => Some(Modality::Image),
        "audio" => Some(Modality::Audio),
        "video" => Some(Modality::Video),
        _ => None,
    }
}

fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE" {
        return Some("audio/wav");
    }
    if bytes.starts_with(b"ID3")
        || bytes.starts_with(&[0xFF, 0xFB])
        || bytes.starts_with(&[0xFF, 0xF3])
        || bytes.starts_with(&[0xFF, 0xF2])
    {
        return Some("audio/mpeg");
    }
    if bytes.starts_with(b"OggS") {
        return Some("audio/ogg");
    }
    if bytes.starts_with(b"fLaC") {
        return Some("audio/flac");
    }
    if bytes.len() >= 8 && &bytes[4..8] == b"ftyp" {
        return Some("video/mp4");
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some("video/webm");
    }
    None
}

fn hex_sha256(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::IngestLimits;
    use llmobs_core::message::messages_to_json;
    use serde_json::json;

    fn limits(min: usize, max: usize) -> IngestLimits {
        IngestLimits {
            media_min_bytes: min,
            media_max_bytes: max,
            ..IngestLimits::default()
        }
    }

    fn encode(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn png() -> Vec<u8> {
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend(b"pixels");
        bytes
    }

    #[test]
    fn each_magic_signature_sets_the_mime_type() {
        let samples: &[(&[u8], &str)] = &[
            (
                &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1],
                "image/png",
            ),
            (&[0xFF, 0xD8, 0xFF, 1, 2, 3], "image/jpeg"),
            (b"GIF87a!!", "image/gif"),
            (b"GIF89a!!", "image/gif"),
            (b"RIFF....WEBP", "image/webp"),
            (b"%PDF-1.4", "application/pdf"),
            (b"RIFF....WAVE", "audio/wav"),
            (b"ID3.......", "audio/mpeg"),
            (&[0xFF, 0xFB, 1, 2, 3, 4], "audio/mpeg"),
            (&[0xFF, 0xF3, 1, 2, 3, 4], "audio/mpeg"),
            (&[0xFF, 0xF2, 1, 2, 3, 4], "audio/mpeg"),
            (b"OggS....", "audio/ogg"),
            (b"fLaC....", "audio/flac"),
            (b"....ftyp", "video/mp4"),
            (&[0x1A, 0x45, 0xDF, 0xA3, 1, 2], "video/webm"),
            (b"unknown!", OCTET_STREAM),
        ];
        let limits = limits(1, 64);
        for (bytes, mime) in samples {
            let mut sink = MediaSink::new(&limits);
            let encoded = encode(bytes);
            let got = sink.extract(&encoded, None);
            let Some(Replacement::Extracted { mime: found, .. }) = got else {
                panic!("expected extraction for {mime}");
            };
            assert_eq!(found, *mime);
        }
    }

    #[test]
    fn a_declared_mime_wins_over_magic_bytes() {
        let limits = limits(1, 64);
        let mut sink = MediaSink::new(&limits);
        let got = sink.extract(&encode(&png()), Some("audio/wav"));
        let Some(Replacement::Extracted { mime, modality, .. }) = got else {
            panic!("expected extraction");
        };
        assert_eq!(mime, "audio/wav");
        assert_eq!(modality, Some(Modality::Audio));
    }

    #[test]
    fn padding_is_subtracted_before_the_size_check() {
        let limits = limits(1, 1);
        let mut sink = MediaSink::new(&limits);
        let got = sink.extract("YQ==", None);
        assert!(matches!(got, Some(Replacement::Extracted { .. })));
        assert_eq!(sink.blobs.len(), 1);
        assert_eq!(
            sink.blobs.values().next().map(|blob| blob.bytes.len()),
            Some(1)
        );
    }

    #[test]
    fn a_blob_over_the_max_is_not_decoded() {
        let limits = limits(1, 4);
        let mut sink = MediaSink::new(&limits);
        let encoded = encode(&[0; 6]);
        assert!(decoded_len(&encoded).is_some_and(|size| size > 4));
        let got = sink.extract(&encoded, None);
        assert!(matches!(got, Some(Replacement::Dropped { size: 6 })));
        assert!(sink.blobs.is_empty());
        assert!(sink.refs.is_empty());
    }

    #[test]
    fn below_the_minimum_and_invalid_base64_stay_inline() {
        let limits = limits(8, 64);
        let mut sink = MediaSink::new(&limits);
        assert!(sink.extract("YQ==", None).is_none());
        assert!(sink.extract("@@@@", None).is_none());
        assert!(sink.extract("YQ== ", None).is_none());
        assert!(sink.extract("YQ =", None).is_none());
        assert!(sink.blobs.is_empty());
    }

    #[test]
    fn the_same_blob_is_stored_once() {
        let limits = limits(1, 64);
        let encoded = encode(&png());
        let mut sink = MediaSink::new(&limits);
        sink.extract(&encoded, Some("image/png"));
        sink.clear_refs();
        sink.extract(&encoded, Some("image/jpeg"));
        assert_eq!(sink.blobs.len(), 1);
        assert_eq!(
            sink.blobs
                .values()
                .next()
                .map(|blob| blob.mime_type.as_str()),
            Some("image/png")
        );
        assert_eq!(sink.refs.len(), 1);
    }

    #[test]
    fn a_blob_part_becomes_a_media_uri() {
        let limits = limits(1, 64);
        let mut sink = MediaSink::new(&limits);
        let mut payload = Payload::Messages(vec![Message {
            role: llmobs_core::message::Role::User,
            name: None,
            parts: vec![Part::Blob {
                modality: Some(Modality::Image),
                mime_type: Some("image/png".to_owned()),
                content: encode(&png()),
                extra: Map::new(),
            }],
            finish_reason: None,
            extra: Map::new(),
        }]);
        extract_payload(&mut payload, &mut sink);
        let Payload::Messages(messages) = payload else {
            panic!("messages");
        };
        let json = messages_to_json(&messages);
        let sha = sink.media_refs().pop().expect("ref");
        assert_eq!(
            json,
            format!(
                r#"[{{"role":"user","parts":[{{"type":"uri","modality":"image","mime_type":"image/png","uri":"media://{sha}"}}]}}]"#
            )
        );
    }

    #[test]
    fn a_data_uri_in_text_is_replaced_and_a_uri_part_is_rewritten() {
        let limits = limits(1, 64);
        let mut sink = MediaSink::new(&limits);
        let encoded = encode(&png());
        let mut text = format!("see data:image/png;base64,{encoded} end");
        replace_text(&mut text, &mut sink);
        let sha = sink.refs.iter().next().expect("ref").clone();
        assert_eq!(text, format!("see media://{sha} end"));

        let mut part = Part::Uri {
            modality: None,
            mime_type: None,
            uri: format!("data:image/png;base64,{encoded}"),
            extra: Map::new(),
        };
        extract_part(&mut part, &mut sink);
        assert_eq!(
            part,
            Part::Uri {
                modality: Some(Modality::Image),
                mime_type: Some("image/png".to_owned()),
                uri: format!("media://{sha}"),
                extra: Map::new(),
            }
        );
    }

    #[test]
    fn provider_shapes_replace_the_base64_string() {
        let limits = limits(1, 64);
        let encoded = encode(&png());
        let shapes = [
            json!({"image_url": {"url": format!("data:image/png;base64,{encoded}")}}),
            json!({"input_audio": {"data": encoded, "format": "wav"}}),
            json!({"source": {"type": "base64", "media_type": "image/png", "data": encoded}}),
            json!({"inline_data": {"mime_type": "image/png", "data": encoded}}),
            json!({"inlineData": {"mimeType": "image/png", "data": encoded}}),
            json!({"type": "image", "image": encoded, "mimeType": "image/png"}),
            json!({"type": "file", "data": encoded, "mediaType": "application/pdf"}),
        ];
        for mut shape in shapes {
            let mut sink = MediaSink::new(&limits);
            extract_json(&mut shape, &mut sink);
            let sha = sink.media_refs().pop().expect("ref");
            let text = serde_json::to_string(&shape).unwrap_or_default();
            assert!(text.contains(&format!("media://{sha}")), "{text}");
            assert!(!text.contains(&encoded), "{text}");
        }
    }

    #[test]
    fn an_oversized_part_uses_the_dropped_marker_and_a_raw_string_does_too() {
        let limits = limits(1, 4);
        let encoded = encode(&[0; 6]);
        let mut part = Part::Blob {
            modality: Some(Modality::Image),
            mime_type: Some("image/png".to_owned()),
            content: encoded.clone(),
            extra: Map::new(),
        };
        let mut sink = MediaSink::new(&limits);
        extract_part(&mut part, &mut sink);
        assert_eq!(
            part,
            Part::Uri {
                modality: None,
                mime_type: None,
                uri: DROPPED_URI.to_owned(),
                extra: Map::from_iter([
                    ("reason".to_owned(), json!("too_large")),
                    ("size".to_owned(), json!(6)),
                ]),
            }
        );
        let mut raw = json!({"input_audio": {"data": encoded}});
        extract_json(&mut raw, &mut sink);
        assert_eq!(raw["input_audio"]["data"], json!(DROPPED_URI));
        assert!(sink.blobs.is_empty());
    }
}
