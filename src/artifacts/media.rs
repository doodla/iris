//! Media sniffing and validation (C-04 "Artifact validation", SPEC §6).
//!
//! * [`sniff`] identifies a media type from magic bytes (never from file names):
//!   PNG, JPEG, WebP, GIF via `infer`; ISO-BMFF files (`ftyp` at offset 4) are
//!   classified by their brands into HEIC/HEIF images or MP4/QuickTime video.
//! * Images of the decodable types (PNG, JPEG, WebP) are fully decoded with the
//!   `image` crate so truncated or corrupt data is rejected and dimensions are known.
//! * Videos are checked with a small ISO-BMFF box walker: the first top-level box
//!   is `ftyp`, a `moov` box is present, every top-level box fits in the file (a
//!   truncated download fails), and the duration is read from `moov/mvhd`.
//!
//! The box walker is hand-rolled on purpose: Iris needs three facts (first box,
//! `moov` presence, `mvhd` duration) and a truncation check. Existing crates are
//! either unmaintained (`mp4`), MPL-licensed (`mp4parse`), or bind to native
//! FFmpeg; ~100 lines of bounds-checked parsing are simpler to audit.

use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{ErrorCode, IrisError};

pub const PNG: &str = "image/png";
pub const JPEG: &str = "image/jpeg";
pub const WEBP: &str = "image/webp";
pub const GIF: &str = "image/gif";
pub const HEIC: &str = "image/heic";
pub const HEIF: &str = "image/heif";
pub const MP4: &str = "video/mp4";
pub const QUICKTIME: &str = "video/quicktime";

/// Every media type Iris can recognize.
pub const KNOWN_MEDIA_TYPES: &[&str] = &[PNG, JPEG, WEBP, GIF, HEIC, HEIF, MP4, QUICKTIME];

/// Largest `moov` box Iris reads into memory to find the duration.
const MAX_MOOV_BYTES: u64 = 64 * 1024 * 1024;
/// Bytes read from the start of a file for sniffing.
const SNIFF_LEN: usize = 64;

/// Facts established about a validated media file.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    /// Media type sniffed from the content (one of [`KNOWN_MEDIA_TYPES`]).
    pub media_type: &'static str,
    /// Pixel dimensions (decoded images only).
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Duration in seconds (videos whose `mvhd` is parseable).
    pub duration_seconds: Option<f64>,
}

/// Details of a decoded image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDetails {
    pub media_type: &'static str,
    pub width: u32,
    pub height: u32,
    /// True if the decoded color type has an alpha channel (PNG `tRNS` counts).
    pub has_alpha: bool,
}

/// Result of walking an ISO-BMFF (MP4/QuickTime) file.
#[derive(Debug, Clone, PartialEq)]
pub struct IsoBmffInfo {
    pub media_type: &'static str,
    /// Major brand from the `ftyp` box, e.g. `isom`.
    pub major_brand: String,
    pub duration_seconds: Option<f64>,
}

/// Identify a media type from the first bytes of content (at least 16 bytes are
/// needed for ISO-BMFF). Returns `None` for anything Iris does not handle.
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let major: [u8; 4] = head[8..12].try_into().ok()?;
        let ftyp_len = u32::from_be_bytes(head[0..4].try_into().ok()?) as usize;
        let end = ftyp_len.min(head.len());
        let compatible = if end > 16 { &head[16..end] } else { &[][..] };
        return classify_brands(&major, compatible);
    }
    let kind = infer::get(head)?;
    match kind.mime_type() {
        "image/png" => Some(PNG),
        "image/jpeg" => Some(JPEG),
        "image/gif" => Some(GIF),
        "image/webp" if head.starts_with(b"RIFF") => Some(WEBP),
        _ => None,
    }
}

/// Media type of an ISO-BMFF file from its `ftyp` brands.
fn classify_brands(major: &[u8; 4], compatible: &[u8]) -> Option<&'static str> {
    let has = |brand: &[u8; 4]| compatible.chunks_exact(4).any(|c| c == brand);
    match major {
        b"qt  " => Some(QUICKTIME),
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" => Some(HEIC),
        b"mif1" | b"msf1" => {
            if has(b"avif") || has(b"avis") {
                None
            } else if has(b"heic") || has(b"heix") {
                Some(HEIC)
            } else {
                Some(HEIF)
            }
        }
        // Still images and audio-only brands Iris does not accept as video.
        b"avif" | b"avis" | b"crx " | b"M4A " | b"M4B " | b"M4P " | b"F4A " | b"F4B " => None,
        // Every other ISO-BMFF brand (isom, iso2..iso9, mp41, mp42, avc1, dash, M4V , ...)
        // is treated as MP4; the box walker then checks the structure.
        _ => Some(MP4),
    }
}

/// Canonical file extension for a media type (`image/jpeg` → `jpg`).
pub fn extension_for(media_type: &str) -> Option<&'static str> {
    Some(match normalize(media_type).as_str() {
        PNG => "png",
        JPEG => "jpg",
        WEBP => "webp",
        GIF => "gif",
        HEIC => "heic",
        HEIF => "heif",
        MP4 => "mp4",
        QUICKTIME => "mov",
        _ => return None,
    })
}

/// Media type implied by a file extension (case-insensitive, without the dot).
pub fn media_type_for_extension(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "png" => PNG,
        "jpg" | "jpeg" => JPEG,
        "webp" => WEBP,
        "gif" => GIF,
        "heic" => HEIC,
        "heif" => HEIF,
        "mp4" | "m4v" => MP4,
        "mov" | "qt" => QUICKTIME,
        _ => return None,
    })
}

pub fn is_image(media_type: &str) -> bool {
    normalize(media_type).starts_with("image/")
}

pub fn is_video(media_type: &str) -> bool {
    normalize(media_type).starts_with("video/")
}

/// Image types Iris fully decodes (dimensions, integrity).
pub fn is_decodable_image(media_type: &str) -> bool {
    image_format(media_type).is_some()
}

/// True if `media_type` is acceptable given a declared list (`image/jpg` is treated
/// as `image/jpeg`, and HEIC/HEIF as one family). An empty list accepts anything.
pub fn accepts(accepted: &[&str], media_type: &str) -> bool {
    if accepted.is_empty() {
        return true;
    }
    let family = |t: &str| {
        let t = normalize(t);
        if t == HEIF { HEIC.to_string() } else { t }
    };
    let wanted = family(media_type);
    accepted.iter().any(|a| family(a) == wanted)
}

/// Lowercase, parameters stripped, `image/jpg` → `image/jpeg`.
fn normalize(media_type: &str) -> String {
    let base = media_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match base.as_str() {
        "image/jpg" | "image/pjpeg" => JPEG.to_string(),
        _ => base,
    }
}

fn image_format(media_type: &str) -> Option<image::ImageFormat> {
    match normalize(media_type).as_str() {
        PNG => Some(image::ImageFormat::Png),
        JPEG => Some(image::ImageFormat::Jpeg),
        WEBP => Some(image::ImageFormat::WebP),
        _ => None,
    }
}

/// Fully decode an image (PNG, JPEG, WebP) held in memory. Errors are `invalid_media`.
pub fn inspect_image(bytes: &[u8]) -> Result<ImageDetails, IrisError> {
    let media_type = sniff(bytes).ok_or_else(|| unrecognized(bytes))?;
    let format = image_format(media_type).ok_or_else(|| {
        invalid_media(format!(
            "content is {media_type}, which Iris cannot decode (decodable: png, jpeg, webp)"
        ))
    })?;
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(image::Limits::default());
    let img = reader
        .decode()
        .map_err(|e| invalid_media(format!("content is not a decodable {media_type} image: {e}")))?;
    Ok(ImageDetails {
        media_type,
        width: img.width(),
        height: img.height(),
        has_alpha: img.color().has_alpha(),
    })
}

/// Decode a PNG and report its dimensions and whether it has an alpha channel
/// (e.g. for OpenAI edit masks). Non-PNG content is `invalid_media`.
pub fn png_info(bytes: &[u8]) -> Result<ImageDetails, IrisError> {
    match sniff(bytes) {
        Some(PNG) => inspect_image(bytes),
        Some(other) => Err(invalid_media(format!("content is {other}, not image/png"))),
        None => Err(unrecognized(bytes)),
    }
}

/// Validate in-memory media: sniff, check against `expected` (empty = any known
/// type), then decode images / walk ISO-BMFF video. Errors are `invalid_media`.
pub fn validate_bytes(bytes: &[u8], expected: &[&str]) -> Result<MediaInfo, IrisError> {
    let media_type = sniff(bytes).ok_or_else(|| unrecognized(bytes))?;
    check_expected(media_type, expected)?;
    if is_decodable_image(media_type) {
        let d = inspect_image(bytes)?;
        return Ok(MediaInfo {
            media_type,
            width: Some(d.width),
            height: Some(d.height),
            duration_seconds: None,
        });
    }
    if is_video(media_type) {
        let info = inspect_iso_bmff(&mut Cursor::new(bytes))
            .map_err(|why| invalid_media(format!("content is not a valid {media_type} video: {why}")))?;
        return Ok(MediaInfo {
            media_type,
            width: None,
            height: None,
            duration_seconds: info.duration_seconds,
        });
    }
    Ok(MediaInfo { media_type, width: None, height: None, duration_seconds: None })
}

/// Validate a media file on disk (see [`validate_bytes`]). Videos are walked
/// without loading them into memory. Errors: `invalid_media`, `io_error`.
pub fn validate_file(path: &Path, expected: &[&str]) -> Result<MediaInfo, IrisError> {
    let mut file =
        File::open(path).map_err(|e| IrisError::io(format_args!("cannot read {}", path.display()), &e))?;
    validate_reader(&mut file, path, expected)
}

/// Validate media read from the start of `reader` (e.g. an open temp file, so it is
/// never reopened by name). `shown` names the content in `io_error` messages.
/// Videos are walked without loading them into memory. Errors: `invalid_media`,
/// `io_error`.
pub fn validate_reader<R: Read + Seek>(
    reader: &mut R,
    shown: &Path,
    expected: &[&str],
) -> Result<MediaInfo, IrisError> {
    let io_err = |e: io::Error| IrisError::io(format_args!("cannot read {}", shown.display()), &e);
    reader.seek(SeekFrom::Start(0)).map_err(io_err)?;
    let mut head = Vec::with_capacity(SNIFF_LEN);
    (&mut *reader).take(SNIFF_LEN as u64).read_to_end(&mut head).map_err(io_err)?;
    let media_type = sniff(&head).ok_or_else(|| unrecognized(&head))?;
    check_expected(media_type, expected)?;
    reader.seek(SeekFrom::Start(0)).map_err(io_err)?;
    if is_video(media_type) {
        let info = inspect_iso_bmff(&mut BufReader::new(reader))
            .map_err(|why| invalid_media(format!("content is not a valid {media_type} video: {why}")))?;
        return Ok(MediaInfo {
            media_type,
            width: None,
            height: None,
            duration_seconds: info.duration_seconds,
        });
    }
    if is_decodable_image(media_type) {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map_err(io_err)?;
        return validate_bytes(&bytes, expected);
    }
    Ok(MediaInfo { media_type, width: None, height: None, duration_seconds: None })
}

fn check_expected(media_type: &str, expected: &[&str]) -> Result<(), IrisError> {
    if accepts(expected, media_type) {
        Ok(())
    } else {
        Err(invalid_media(format!("content is {media_type}, expected {}", expected.join(" or "))))
    }
}

fn invalid_media(message: String) -> IrisError {
    IrisError::new(ErrorCode::InvalidMedia, message)
}

/// Error for content that is not a recognized media type, with a hint when it looks
/// like an API error body. Never includes the content itself.
fn unrecognized(head: &[u8]) -> IrisError {
    let first = head.iter().copied().find(|b| !b.is_ascii_whitespace());
    let looks_like = match first {
        None => " (it is empty)",
        Some(b'{') | Some(b'[') => " (it looks like a JSON document, e.g. an API error body)",
        Some(b'<') => " (it looks like HTML or XML text, e.g. an error page)",
        Some(_) if std::str::from_utf8(head).is_ok() => " (it looks like plain text)",
        Some(_) => "",
    };
    invalid_media(format!("content is not a recognized image or video format{looks_like}"))
}

/// Walk the top-level boxes of an ISO-BMFF file (MP4/QuickTime).
///
/// Checks that the first box is `ftyp` with a video brand, that every box header is
/// sane and every box fits inside the file (so truncated files fail), and that a
/// `moov` box is present. Reports the duration from `moov/mvhd` when parseable.
/// Returns a human-readable reason on failure.
pub fn inspect_iso_bmff<R: Read + Seek>(reader: &mut R) -> Result<IsoBmffInfo, String> {
    let io = |e: io::Error| format!("read error: {e}");
    let len = reader.seek(SeekFrom::End(0)).map_err(io)?;
    reader.seek(SeekFrom::Start(0)).map_err(io)?;
    if len == 0 {
        return Err("the file is empty".to_string());
    }

    let mut pos = 0u64;
    let mut ftyp: Option<(&'static str, String)> = None;
    let mut saw_moov = false;
    let mut duration = None;
    while pos < len {
        let header = read_box_header(reader, pos, len)?;
        let kind = header.kind_str();
        if ftyp.is_none() {
            if &header.kind != b"ftyp" {
                return Err(format!("the first box is '{kind}', not 'ftyp'"));
            }
            let payload = read_payload(reader, &header, 4096)?;
            if payload.len() < 8 {
                return Err("the 'ftyp' box is too short".to_string());
            }
            let major: [u8; 4] = payload[0..4].try_into().map_err(|_| "bad ftyp")?;
            let brand = String::from_utf8_lossy(&major).into_owned();
            let media_type = classify_brands(&major, &payload[8..])
                .filter(|t| is_video(t))
                .ok_or_else(|| format!("the 'ftyp' brand '{brand}' is not a video brand"))?;
            ftyp = Some((media_type, brand));
        } else if &header.kind == b"moov" {
            saw_moov = true;
            if duration.is_none() && header.payload_len() <= MAX_MOOV_BYTES {
                let payload = read_payload(reader, &header, MAX_MOOV_BYTES)?;
                duration = mvhd_duration(&payload);
            }
        }
        pos += header.size;
        reader.seek(SeekFrom::Start(pos)).map_err(io)?;
    }
    let (media_type, major_brand) = ftyp.ok_or("no 'ftyp' box")?;
    if !saw_moov {
        return Err(
            "no 'moov' box (movie metadata) was found; the file is incomplete or not playable".to_string()
        );
    }
    Ok(IsoBmffInfo { media_type, major_brand, duration_seconds: duration })
}

struct BoxHeader {
    kind: [u8; 4],
    /// Offset of the box in the file.
    offset: u64,
    header_len: u64,
    /// Total box size including the header.
    size: u64,
}

impl BoxHeader {
    fn payload_len(&self) -> u64 {
        self.size - self.header_len
    }

    fn kind_str(&self) -> String {
        self.kind.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '?' }).collect()
    }
}

fn read_box_header<R: Read + Seek>(reader: &mut R, pos: u64, len: u64) -> Result<BoxHeader, String> {
    let remaining = len - pos;
    if remaining < 8 {
        return Err(format!("truncated box header at offset {pos} ({remaining} trailing bytes)"));
    }
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf).map_err(|e| format!("read error at offset {pos}: {e}"))?;
    let size32 = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let kind = [buf[4], buf[5], buf[6], buf[7]];
    if !kind.iter().all(|b| (0x20..0x7f).contains(b)) {
        return Err(format!("invalid box type at offset {pos}; this is not an ISO-BMFF (MP4) structure"));
    }
    let (header_len, size) = match size32 {
        0 => (8, remaining),
        1 => {
            if remaining < 16 {
                return Err(format!("truncated 64-bit box header at offset {pos}"));
            }
            let mut large = [0u8; 8];
            reader.read_exact(&mut large).map_err(|e| format!("read error at offset {pos}: {e}"))?;
            (16, u64::from_be_bytes(large))
        }
        n => (8, u64::from(n)),
    };
    let header = BoxHeader { kind, offset: pos, header_len, size };
    if size < header_len {
        return Err(format!("box '{}' at offset {pos} has an invalid size {size}", header.kind_str()));
    }
    if size > remaining {
        return Err(format!(
            "box '{}' at offset {pos} needs {size} bytes but only {remaining} remain (truncated file?)",
            header.kind_str()
        ));
    }
    Ok(header)
}

/// Read up to `max` bytes of a box's payload (the reader is positioned after the header).
fn read_payload<R: Read + Seek>(reader: &mut R, header: &BoxHeader, max: u64) -> Result<Vec<u8>, String> {
    let want = header.payload_len().min(max);
    let mut payload = Vec::with_capacity(want as usize);
    reader
        .seek(SeekFrom::Start(header.offset + header.header_len))
        .and_then(|_| reader.take(want).read_to_end(&mut payload))
        .map_err(|e| format!("read error in box '{}': {e}", header.kind_str()))?;
    if (payload.len() as u64) < want {
        return Err(format!("box '{}' is truncated", header.kind_str()));
    }
    Ok(payload)
}

/// Duration from the `mvhd` child of a `moov` payload, if present and meaningful.
fn mvhd_duration(moov: &[u8]) -> Option<f64> {
    let mut pos = 0usize;
    while pos + 8 <= moov.len() {
        let size32 = u32::from_be_bytes(moov[pos..pos + 4].try_into().ok()?) as u64;
        let kind = &moov[pos + 4..pos + 8];
        let (header_len, size) = match size32 {
            0 => (8u64, (moov.len() - pos) as u64),
            1 => {
                let large = moov.get(pos + 8..pos + 16)?;
                (16, u64::from_be_bytes(large.try_into().ok()?))
            }
            n => (8, n),
        };
        if size < header_len || pos as u64 + size > moov.len() as u64 {
            return None;
        }
        if kind == b"mvhd" {
            let body = &moov[pos + header_len as usize..pos + size as usize];
            let version = *body.first()?;
            let (timescale, duration) = if version == 1 {
                let ts = u32::from_be_bytes(body.get(20..24)?.try_into().ok()?);
                let d = u64::from_be_bytes(body.get(24..32)?.try_into().ok()?);
                (ts, if d == u64::MAX { return None } else { d })
            } else {
                let ts = u32::from_be_bytes(body.get(12..16)?.try_into().ok()?);
                let d = u32::from_be_bytes(body.get(16..20)?.try_into().ok()?);
                (ts, if d == u32::MAX { return None } else { u64::from(d) })
            };
            if timescale == 0 {
                return None;
            }
            let seconds = duration as f64 / f64::from(timescale);
            return Some((seconds * 1000.0).round() / 1000.0);
        }
        pos += size as usize;
    }
    None
}
