//! OTLP request bodies: format and encoding from headers, capped reads, and
//! capped decompression (spec 02 § Endpoints, D-G-2, D-G-4, D-G-5).

use std::io::Read;

use axum::body::Body;
use axum::http::{HeaderMap, header};
use bytes::Bytes;
use flate2::read::{MultiGzDecoder, ZlibDecoder};
use http_body_util::{BodyExt, LengthLimitError, Limited};

/// Decompression reads at most this much per step, so the reservation grows in small steps.
const CHUNK_BYTES: usize = 64 * 1024;

/// zstd window cap: `2^24` bytes (16 MiB). A larger window would let one frame make the
/// decoder allocate up to 128 MiB, outside what the in-flight budget covers.
const ZSTD_WINDOW_LOG_MAX: u32 = 24;

/// OTLP body encoding from `Content-Type` (spec 02 § Endpoints).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyFormat {
    Protobuf,
    Json,
}

/// `Content-Encoding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyEncoding {
    Identity,
    Gzip,
    Deflate,
    Zstd,
}

/// Why a body was refused. Mapped to `ApiError` by the route (G1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyError {
    /// 415 `unsupported_media_type`, detail "expected Content-Type application/x-protobuf or application/json".
    UnsupportedType,
    /// 415, detail "unsupported Content-Encoding".
    UnsupportedEncoding,
    /// 413, detail "request body is too large".
    TooLarge,
    /// 400 `bad_request`, detail "invalid compressed body".
    Corrupt,
    /// 400, detail "could not read the request body" (client disconnect, transport error).
    Read,
}

/// Grows the caller's budget reservation; `false` means the budget is full.
pub(crate) type Reserve<'a> = &'a mut dyn FnMut(usize) -> bool;

/// Why decompression stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecompressError {
    /// A body error (`TooLarge` or `Corrupt`).
    Body(BodyError),
    /// `reserve` returned `false`: the in-flight budget is full (503).
    Budget,
}

/// The body format from `Content-Type`. Parameters such as `charset` are ignored.
pub(crate) fn body_format(headers: &HeaderMap) -> Result<BodyFormat, BodyError> {
    let value = headers
        .get(header::CONTENT_TYPE)
        .ok_or(BodyError::UnsupportedType)?;
    let text = value.to_str().map_err(|_| BodyError::UnsupportedType)?;
    let media_type = text.split(';').next().unwrap_or_default().trim();
    if media_type.eq_ignore_ascii_case("application/x-protobuf") {
        Ok(BodyFormat::Protobuf)
    } else if media_type.eq_ignore_ascii_case("application/json") {
        Ok(BodyFormat::Json)
    } else {
        Err(BodyError::UnsupportedType)
    }
}

/// The body encoding from `Content-Encoding`. A missing header or `identity` is no encoding.
pub(crate) fn body_encoding(headers: &HeaderMap) -> Result<BodyEncoding, BodyError> {
    let mut values = headers.get_all(header::CONTENT_ENCODING).iter();
    let Some(value) = values.next() else {
        return Ok(BodyEncoding::Identity);
    };
    if values.next().is_some() {
        return Err(BodyError::UnsupportedEncoding);
    }
    let text = value
        .to_str()
        .map_err(|_| BodyError::UnsupportedEncoding)?
        .trim();
    if text.eq_ignore_ascii_case("identity") {
        Ok(BodyEncoding::Identity)
    } else if text.eq_ignore_ascii_case("gzip") || text.eq_ignore_ascii_case("x-gzip") {
        Ok(BodyEncoding::Gzip)
    } else if text.eq_ignore_ascii_case("deflate") {
        Ok(BodyEncoding::Deflate)
    } else if text.eq_ignore_ascii_case("zstd") {
        Ok(BodyEncoding::Zstd)
    } else {
        Err(BodyError::UnsupportedEncoding)
    }
}

/// Collects the body, refusing more than `max` bytes. A declared `Content-Length` above
/// `max` is refused without reading.
pub(crate) async fn read_capped(
    body: Body,
    headers: &HeaderMap,
    max: usize,
) -> Result<Bytes, BodyError> {
    let limit = u64::try_from(max).unwrap_or(u64::MAX);
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<u64>().ok());
    if declared.is_some_and(|length| length > limit) {
        return Err(BodyError::TooLarge);
    }
    match Limited::new(body, max).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) if error.is::<LengthLimitError>() => Err(BodyError::TooLarge),
        Err(_) => Err(BodyError::Read),
    }
}

/// Decompresses `raw` with `encoding`. The output is at most `max` bytes. Before each chunk
/// joins the output, `reserve` is asked for its length, so the memory held is always
/// covered by the caller's in-flight budget. Blocking: call it from `spawn_blocking`.
pub(crate) fn decompress(
    raw: Bytes,
    encoding: BodyEncoding,
    max: usize,
    reserve: Reserve<'_>,
) -> Result<Bytes, DecompressError> {
    let mut reader: Box<dyn Read + '_> = match encoding {
        BodyEncoding::Identity => return Ok(raw),
        BodyEncoding::Gzip => Box::new(MultiGzDecoder::new(raw.as_ref())),
        BodyEncoding::Deflate => Box::new(ZlibDecoder::new(raw.as_ref())),
        BodyEncoding::Zstd => {
            let mut decoder = zstd::stream::read::Decoder::new(raw.as_ref())
                .map_err(|_| DecompressError::Body(BodyError::Corrupt))?;
            decoder
                .window_log_max(ZSTD_WINDOW_LOG_MAX)
                .map_err(|_| DecompressError::Body(BodyError::Corrupt))?;
            Box::new(decoder)
        }
    };
    let mut output = Vec::new();
    let mut chunk = vec![0_u8; CHUNK_BYTES];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|_| DecompressError::Body(BodyError::Corrupt))?;
        if read == 0 {
            break;
        }
        if output.len().saturating_add(read) > max {
            return Err(DecompressError::Body(BodyError::TooLarge));
        }
        if !reserve(read) {
            return Err(DecompressError::Budget);
        }
        output.extend_from_slice(&chunk[..read]);
    }
    Ok(Bytes::from(output))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use axum::http::{HeaderName, HeaderValue};
    use flate2::Compression;
    use flate2::write::{GzEncoder, ZlibEncoder};

    use super::*;

    const MIB: usize = 1024 * 1024;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).expect("test header name"),
                HeaderValue::from_str(value).expect("test header value"),
            );
        }
        map
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).expect("gzip write");
        encoder.finish().expect("gzip finish")
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).expect("zlib write");
        encoder.finish().expect("zlib finish")
    }

    fn zstd_bytes(data: &[u8]) -> Vec<u8> {
        zstd::stream::encode_all(data, 0).expect("zstd encode")
    }

    /// Runs `decompress` with a reservation that always succeeds and returns the output and
    /// the sum of every reserved length.
    fn decompress_counting(
        raw: Vec<u8>,
        encoding: BodyEncoding,
        max: usize,
    ) -> (Result<Bytes, DecompressError>, usize) {
        let mut reserved = 0_usize;
        let mut reserve = |bytes: usize| {
            reserved += bytes;
            true
        };
        let result = decompress(Bytes::from(raw), encoding, max, &mut reserve);
        (result, reserved)
    }

    #[test]
    fn content_types_are_recognised_ignoring_parameters_and_case() {
        let cases = [
            ("application/x-protobuf", BodyFormat::Protobuf),
            ("application/json; charset=utf-8", BodyFormat::Json),
            ("APPLICATION/JSON", BodyFormat::Json),
        ];
        for (value, expected) in cases {
            let map = headers(&[("content-type", value)]);
            assert_eq!(body_format(&map), Ok(expected), "{value}");
        }
    }

    #[test]
    fn other_or_missing_content_types_are_refused() {
        for value in ["text/plain", "application/protobuf", ""] {
            let map = headers(&[("content-type", value)]);
            assert_eq!(
                body_format(&map),
                Err(BodyError::UnsupportedType),
                "{value}"
            );
        }
        assert_eq!(
            body_format(&HeaderMap::new()),
            Err(BodyError::UnsupportedType)
        );
    }

    #[test]
    fn non_ascii_content_type_is_refused() {
        let mut map = HeaderMap::new();
        map.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_bytes("application/json\u{e9}".as_bytes()).expect("opaque bytes"),
        );
        assert_eq!(body_format(&map), Err(BodyError::UnsupportedType));
    }

    #[test]
    fn every_content_encoding_spelling_is_accepted() {
        let cases = [
            ("identity", BodyEncoding::Identity),
            ("gzip", BodyEncoding::Gzip),
            (" X-Gzip ", BodyEncoding::Gzip),
            ("deflate", BodyEncoding::Deflate),
            ("ZSTD", BodyEncoding::Zstd),
        ];
        for (value, expected) in cases {
            let map = headers(&[("content-encoding", value)]);
            assert_eq!(body_encoding(&map), Ok(expected), "{value}");
        }
        assert_eq!(body_encoding(&HeaderMap::new()), Ok(BodyEncoding::Identity));
    }

    #[test]
    fn unsupported_or_repeated_content_encodings_are_refused() {
        for value in ["br", "gzip, zstd", ""] {
            let map = headers(&[("content-encoding", value)]);
            assert_eq!(
                body_encoding(&map),
                Err(BodyError::UnsupportedEncoding),
                "{value}"
            );
        }
        let map = headers(&[("content-encoding", "gzip"), ("content-encoding", "gzip")]);
        assert_eq!(body_encoding(&map), Err(BodyError::UnsupportedEncoding));
    }

    fn stream_body(chunks: Vec<Result<Bytes, std::io::Error>>) -> Body {
        Body::from_stream(futures::stream::iter(chunks))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_capped_accepts_exactly_the_limit() {
        let body = Body::from(vec![7_u8; 16]);
        let read = read_capped(body, &HeaderMap::new(), 16).await;
        assert_eq!(read.map(|bytes| bytes.len()), Ok(16));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_capped_refuses_one_byte_over_the_limit() {
        let body = Body::from(vec![7_u8; 17]);
        let read = read_capped(body, &HeaderMap::new(), 16).await;
        assert_eq!(read, Err(BodyError::TooLarge));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_capped_refuses_a_declared_length_over_the_limit_without_reading() {
        let body = stream_body(vec![Ok(Bytes::from_static(b"ab"))]);
        let map = headers(&[("content-length", "17")]);
        let read = read_capped(body, &map, 16).await;
        assert_eq!(read, Err(BodyError::TooLarge));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_capped_reports_a_failing_stream_as_read_error() {
        let body = stream_body(vec![
            Ok(Bytes::from_static(b"ab")),
            Err(std::io::Error::other("boom")),
        ]);
        let read = read_capped(body, &HeaderMap::new(), 16).await;
        assert_eq!(read, Err(BodyError::Read));
    }

    #[test]
    fn decompress_identity_returns_the_bytes_and_reserves_nothing() {
        let (result, reserved) = decompress_counting(b"plain".to_vec(), BodyEncoding::Identity, 16);
        assert_eq!(result.as_deref(), Ok(&b"plain"[..]));
        assert_eq!(reserved, 0);
    }

    #[test]
    fn decompress_round_trips_gzip_including_concatenated_members() {
        let data = b"resourceSpans payload".repeat(1000);
        let (result, reserved) = decompress_counting(gzip(&data), BodyEncoding::Gzip, MIB);
        assert_eq!(result.as_deref(), Ok(&data[..]));
        assert!(reserved >= data.len());

        let mut two_members = gzip(b"first, ");
        two_members.extend(gzip(b"second"));
        let (result, _) = decompress_counting(two_members, BodyEncoding::Gzip, MIB);
        assert_eq!(result.as_deref(), Ok(&b"first, second"[..]));
    }

    #[test]
    fn decompress_round_trips_deflate_and_zstd() {
        let data = b"span attributes ".repeat(4000);

        let (result, reserved) = decompress_counting(zlib(&data), BodyEncoding::Deflate, MIB);
        assert_eq!(result.as_deref(), Ok(&data[..]));
        assert!(reserved >= data.len());

        let (result, reserved) = decompress_counting(zstd_bytes(&data), BodyEncoding::Zstd, MIB);
        assert_eq!(result.as_deref(), Ok(&data[..]));
        assert!(reserved >= data.len());
    }

    #[test]
    fn decompress_refuses_a_bomb_at_the_limit() {
        let max = MIB;
        let bomb = gzip(&vec![0_u8; 64 * MIB]);
        assert!(bomb.len() < max, "the bomb itself must be small");
        let (result, reserved) = decompress_counting(bomb, BodyEncoding::Gzip, max);
        assert_eq!(result, Err(DecompressError::Body(BodyError::TooLarge)));
        assert!(
            reserved <= max,
            "reserved {reserved} bytes for a {max}-byte limit"
        );
    }

    #[test]
    fn decompress_stops_when_the_budget_refuses() {
        let data = vec![0_u8; MIB];
        let mut total = 0_usize;
        let mut reserve = |bytes: usize| {
            total += bytes;
            total <= 128 * 1024
        };
        let result = decompress(
            Bytes::from(gzip(&data)),
            BodyEncoding::Gzip,
            4 * MIB,
            &mut reserve,
        );
        assert_eq!(result, Err(DecompressError::Budget));
    }

    #[test]
    fn decompress_refuses_truncated_or_garbage_input_as_corrupt() {
        let data = b"some otlp bytes".repeat(500);
        let mut truncated = gzip(&data);
        truncated.truncate(truncated.len() / 2);
        let corrupt = |raw: Vec<u8>, encoding| decompress_counting(raw, encoding, MIB).0;
        assert_eq!(
            corrupt(truncated, BodyEncoding::Gzip),
            Err(DecompressError::Body(BodyError::Corrupt))
        );
        assert_eq!(
            corrupt(b"not gzip at all".to_vec(), BodyEncoding::Gzip),
            Err(DecompressError::Body(BodyError::Corrupt))
        );
        assert_eq!(
            corrupt(b"not zlib at all".to_vec(), BodyEncoding::Deflate),
            Err(DecompressError::Body(BodyError::Corrupt))
        );
        assert_eq!(
            corrupt(b"not zstd at all".to_vec(), BodyEncoding::Zstd),
            Err(DecompressError::Body(BodyError::Corrupt))
        );
    }

    #[test]
    fn decompress_refuses_a_zstd_window_above_16_mib_as_corrupt() {
        // A 64 MiB input with `window_log(27)` and long-distance matching writes a frame whose
        // window is 128 MiB, above the 16 MiB cap.
        let data = vec![0_u8; 64 * MIB];
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).expect("zstd encoder");
        encoder.window_log(27).expect("window_log");
        encoder.long_distance_matching(true).expect("ldm");
        encoder.write_all(&data).expect("zstd write");
        let frame = encoder.finish().expect("zstd finish");

        let (result, _) = decompress_counting(frame, BodyEncoding::Zstd, 128 * MIB);
        assert_eq!(result, Err(DecompressError::Body(BodyError::Corrupt)));
    }
}
