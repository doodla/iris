//! Media sniffing and validation (see docs/jobs.md and docs/configuration.md#security-rules).
//!
//! * [`sniff`] identifies a media type from magic bytes (never from file names):
//!   PNG, JPEG, WebP, GIF via `infer`; ISO-BMFF files (`ftyp` at offset 4) are
//!   classified by their brands into HEIC/HEIF images or MP4/QuickTime video.
//! * Images of the decodable types (PNG, JPEG, WebP) are fully decoded with the
//!   `image` crate so truncated or corrupt data is rejected and dimensions are known.
//! * GIF and HEIC/HEIF have no decoder in this build (the `image` crate is built
//!   with png/jpeg/webp only), so their structure is walked instead: GIF
//!   blocks up to the `0x3B` trailer ([`inspect_gif`]), and HEIC/HEIF boxes with
//!   the ISO-BMFF walker ([`inspect_heif`]). Truncated files fail either way.
//! * Videos are checked with a small ISO-BMFF box walker: the first top-level box
//!   is `ftyp`, a `moov` box (movie metadata) and a top-level `mdat` box (media
//!   data; fragmented files carry `moof` + `mdat` pairs) are present, every
//!   top-level box fits in the file, and every chunk offset of the sample tables
//!   (`stco`/`co64`) points inside the file, so a download cut off anywhere,
//!   including right after the metadata, fails. The duration is read from
//!   `moov/mvhd`.
//!
//! No sniffed type is accepted without one of these checks.
//!
//! The box walker is hand-rolled on purpose: Iris needs a few facts (first box,
//! `moov`/`mdat`/`meta` presence, `mvhd` duration, chunk offsets) and a truncation
//! check. Existing
//! crates are either unmaintained (`mp4`), MPL-licensed (`mp4parse`), or bind to
//! native FFmpeg; ~100 lines of bounds-checked parsing are simpler to audit.
//! Every size and offset read from a file is compared with what is left before it
//! is used, so a crafted size (a 64-bit `largesize` near `u64::MAX`, say) is an
//! error or ends the walk, never an overflow, a panic, or an endless loop.

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
    /// Pixel dimensions (decoded images; GIF: the logical screen; videos: the
    /// first video track's `tkhd` size when present; `None` for HEIC/HEIF).
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
    /// Width and height of the first video track (`moov/trak` whose `hdlr` is
    /// `vide`), from its `tkhd`, when parseable.
    pub width: Option<u32>,
    pub height: Option<u32>,
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
/// type), then decode PNG/JPEG/WebP, walk GIF blocks, or walk ISO-BMFF boxes
/// (HEIC/HEIF, video). Errors are `invalid_media`.
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
    if media_type == GIF {
        let (width, height) = inspect_gif(bytes)
            .map_err(|why| invalid_media(format!("content is not a valid {media_type} image: {why}")))?;
        return Ok(MediaInfo {
            media_type,
            width: Some(width),
            height: Some(height),
            duration_seconds: None,
        });
    }
    if media_type == HEIC || media_type == HEIF {
        inspect_heif(&mut Cursor::new(bytes))
            .map_err(|why| invalid_media(format!("content is not a valid {media_type} image: {why}")))?;
        return Ok(MediaInfo { media_type, width: None, height: None, duration_seconds: None });
    }
    if is_video(media_type) {
        let info = inspect_iso_bmff(&mut Cursor::new(bytes))
            .map_err(|why| invalid_media(format!("content is not a valid {media_type} video: {why}")))?;
        return Ok(MediaInfo {
            media_type,
            width: info.width,
            height: info.height,
            duration_seconds: info.duration_seconds,
        });
    }
    Err(invalid_media(format!("Iris cannot validate {media_type} content")))
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
            width: info.width,
            height: info.height,
            duration_seconds: info.duration_seconds,
        });
    }
    // Images are small: validate them in memory.
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(io_err)?;
    validate_bytes(&bytes, expected)
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

/// Walk the block structure of a GIF: header, logical screen descriptor, color
/// tables, extensions, and image descriptors with their LZW data sub-blocks, up to
/// the `0x3B` trailer. Every block must fit in the data and at least one image must
/// be present, so truncated files fail. The LZW data is not decoded (no GIF
/// decoder in this build). Returns the logical screen width and height.
pub fn inspect_gif(bytes: &[u8]) -> Result<(u32, u32), String> {
    if !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Err("missing GIF87a/GIF89a header".to_string());
    }
    let screen = bytes.get(6..13).ok_or("truncated logical screen descriptor")?;
    let width = u16::from_le_bytes([screen[0], screen[1]]);
    let height = u16::from_le_bytes([screen[2], screen[3]]);
    if width == 0 || height == 0 {
        return Err(format!("invalid logical screen size {width}x{height}"));
    }
    let mut pos = 13usize;
    if screen[4] & 0x80 != 0 {
        pos = gif_skip(bytes, pos, gif_color_table_len(screen[4]), "global color table")?;
    }
    let mut images = 0u32;
    loop {
        let Some(&block) = bytes.get(pos) else {
            return Err(format!("truncated after {images} image(s): the 0x3B trailer is missing"));
        };
        pos += 1;
        match block {
            // Extension: label byte, then data sub-blocks.
            0x21 => {
                pos = gif_skip(bytes, pos, 1, "extension label")?;
                pos = gif_skip_sub_blocks(bytes, pos)?;
            }
            // Image: descriptor (left, top, width, height, flags), optional local
            // color table, LZW minimum code size, data sub-blocks.
            0x2C => {
                let flags = *bytes.get(pos + 8).ok_or("truncated image descriptor")?;
                pos += 9;
                if flags & 0x80 != 0 {
                    pos = gif_skip(bytes, pos, gif_color_table_len(flags), "local color table")?;
                }
                let min_code_size = *bytes.get(pos).ok_or("truncated image data")?;
                if min_code_size > 11 {
                    return Err(format!("invalid LZW minimum code size {min_code_size}"));
                }
                pos = gif_skip_sub_blocks(bytes, pos + 1)?;
                images += 1;
            }
            0x3B if images == 0 => return Err("the file contains no image".to_string()),
            0x3B => return Ok((u32::from(width), u32::from(height))),
            other => return Err(format!("unexpected block 0x{other:02x} at offset {}", pos - 1)),
        }
    }
}

fn gif_color_table_len(flags: u8) -> usize {
    3 * (1usize << ((flags & 0x07) + 1))
}

fn gif_skip(bytes: &[u8], pos: usize, len: usize, what: &str) -> Result<usize, String> {
    match pos.checked_add(len) {
        Some(end) if end <= bytes.len() => Ok(end),
        _ => Err(format!("truncated {what} at offset {pos}")),
    }
}

fn gif_skip_sub_blocks(bytes: &[u8], mut pos: usize) -> Result<usize, String> {
    loop {
        let size = *bytes.get(pos).ok_or_else(|| format!("truncated data sub-block at offset {pos}"))?;
        pos += 1;
        if size == 0 {
            return Ok(pos);
        }
        pos = gif_skip(bytes, pos, usize::from(size), "data sub-block")?;
    }
}

/// Walk the top-level boxes of an ISO-BMFF file (MP4/QuickTime).
///
/// Checks that the first box is `ftyp` with a video brand, that every box header is
/// sane and every box fits inside the file (so truncated files fail), that a
/// `moov` box and a top-level `mdat` box (the media data) are present, and that
/// every chunk offset in the sample tables (`stco`/`co64`) lies inside the file
/// (so a file cut inside or before its media data fails even when the last box
/// claims to extend to the end of the file). Reports the duration from
/// `moov/mvhd` when parseable. Returns a human-readable reason on failure.
pub fn inspect_iso_bmff<R: Read + Seek>(reader: &mut R) -> Result<IsoBmffInfo, String> {
    walk_iso_bmff(reader, BmffKind::Video)
}

/// Walk the top-level boxes of a HEIC/HEIF still image: the first box is `ftyp`
/// with a HEIC/HEIF brand, every box fits inside the file (so truncated files
/// fail), and a `meta` box (the image items) is present; image sequences may carry
/// a `moov` box instead. Pixels are not decoded and no dimensions are reported.
pub fn inspect_heif<R: Read + Seek>(reader: &mut R) -> Result<IsoBmffInfo, String> {
    walk_iso_bmff(reader, BmffKind::Image)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BmffKind {
    Video,
    Image,
}

fn walk_iso_bmff<R: Read + Seek>(reader: &mut R, want: BmffKind) -> Result<IsoBmffInfo, String> {
    let io = |e: io::Error| format!("read error: {e}");
    let len = reader.seek(SeekFrom::End(0)).map_err(io)?;
    reader.seek(SeekFrom::Start(0)).map_err(io)?;
    if len == 0 {
        return Err("the file is empty".to_string());
    }

    let mut pos = 0u64;
    let mut ftyp: Option<(&'static str, String)> = None;
    let mut saw_moov = false;
    let mut saw_mdat = false;
    let mut saw_meta = false;
    let mut duration = None;
    let mut dimensions = None;
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
            let (wanted, what): (fn(&str) -> bool, &str) = match want {
                BmffKind::Video => (is_video, "a video"),
                BmffKind::Image => (is_image, "a HEIC/HEIF image"),
            };
            let media_type = classify_brands(&major, &payload[8..])
                .filter(|t| wanted(t))
                .ok_or_else(|| format!("the 'ftyp' brand '{brand}' is not {what} brand"))?;
            ftyp = Some((media_type, brand));
        } else if &header.kind == b"meta" {
            saw_meta = true;
        } else if &header.kind == b"mdat" {
            saw_mdat = true;
        } else if &header.kind == b"moov" {
            saw_moov = true;
            if want == BmffKind::Video && duration.is_none() && header.payload_len() <= MAX_MOOV_BYTES {
                let payload = read_payload(reader, &header, MAX_MOOV_BYTES)?;
                check_chunk_offsets(&payload, len)?;
                duration = mvhd_duration(&payload);
                dimensions = video_dimensions(&payload);
            }
        }
        // `read_box_header` guarantees the box ends inside the file (and after its
        // header), so this always moves forward and never past `len`.
        pos = header.end();
        reader.seek(SeekFrom::Start(pos)).map_err(io)?;
    }
    let (media_type, major_brand) = ftyp.ok_or("no 'ftyp' box")?;
    match want {
        BmffKind::Video if !saw_moov => {
            return Err("no 'moov' box (movie metadata) was found; the file is incomplete or not playable"
                .to_string());
        }
        BmffKind::Video if !saw_mdat => {
            return Err(
                "no 'mdat' box (media data) was found; the file is incomplete (truncated) or not playable"
                    .to_string(),
            );
        }
        BmffKind::Image if !saw_meta && !saw_moov => {
            return Err(
                "no 'meta' box (image items) was found; the file is incomplete or not an image".to_string()
            );
        }
        _ => {}
    }
    Ok(IsoBmffInfo {
        media_type,
        major_brand,
        duration_seconds: duration,
        width: dimensions.map(|(w, _)| w),
        height: dimensions.map(|(_, h)| h),
    })
}

/// A validated box header: `header_len <= size` and `offset + size` is inside the
/// file, so none of the accessors below can overflow.
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
        self.size.saturating_sub(self.header_len)
    }

    /// Offset of the first payload byte.
    fn payload_start(&self) -> u64 {
        self.offset.saturating_add(self.header_len)
    }

    /// Offset just past the box.
    fn end(&self) -> u64 {
        self.offset.saturating_add(self.size)
    }

    fn kind_str(&self) -> String {
        self.kind.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '?' }).collect()
    }
}

/// Read and check the box header at `pos` (the reader is positioned there). Every
/// size comes from the file, so all arithmetic is checked: a size that does not
/// cover its own header, or that reaches past the end of the file (including
/// 64-bit sizes near `u64::MAX`), is an error.
fn read_box_header<R: Read + Seek>(reader: &mut R, pos: u64, len: u64) -> Result<BoxHeader, String> {
    let remaining =
        len.checked_sub(pos).ok_or_else(|| format!("box offset {pos} is past the end of the file"))?;
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

/// Read up to `max` bytes of a box's payload.
fn read_payload<R: Read + Seek>(reader: &mut R, header: &BoxHeader, max: u64) -> Result<Vec<u8>, String> {
    let want = header.payload_len().min(max);
    let mut payload = Vec::with_capacity(usize::try_from(want).unwrap_or(0));
    reader
        .seek(SeekFrom::Start(header.payload_start()))
        .and_then(|_| reader.take(want).read_to_end(&mut payload))
        .map_err(|e| format!("read error in box '{}': {e}", header.kind_str()))?;
    if (payload.len() as u64) < want {
        return Err(format!("box '{}' is truncated", header.kind_str()));
    }
    Ok(payload)
}

/// Direct child boxes of a box payload as `(type, body)`; stops at the first
/// malformed header (callers treat missing data as "unknown").
///
/// Sizes come from the file: each one is compared with what is left of the
/// payload before anything is sliced, so no size (a 64-bit `largesize` near
/// `u64::MAX` included) can overflow an offset, and every step consumes at least
/// one header, so the walk always ends.
fn child_boxes(payload: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut rest = payload;
    while let Some(head) = rest.get(..8) {
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let kind = [head[4], head[5], head[6], head[7]];
        let (header_len, size) = match size32 {
            0 => (8usize, rest.len() as u64),
            1 => match rest.get(8..16).and_then(|b| <[u8; 8]>::try_from(b).ok()) {
                Some(large) => (16, u64::from_be_bytes(large)),
                None => break,
            },
            n => (8, u64::from(n)),
        };
        let Ok(size) = usize::try_from(size) else { break };
        if size < header_len || size > rest.len() {
            break;
        }
        let (this, next) = rest.split_at(size);
        out.push((kind, &this[header_len..]));
        rest = next;
    }
    out
}

/// Pixel size of the first video track in a `moov` payload: the `trak` whose
/// `mdia/hdlr` handler type is `vide`, read from its `tkhd` (16.16 fixed-point
/// width and height in the box's last 8 bytes). `None` when absent or zero.
fn video_dimensions(moov: &[u8]) -> Option<(u32, u32)> {
    child_boxes(moov).into_iter().filter(|(kind, _)| kind == b"trak").find_map(|(_, trak)| {
        let children = child_boxes(trak);
        let is_video = children.iter().filter(|(k, _)| k == b"mdia").any(|(_, mdia)| {
            child_boxes(mdia)
                .iter()
                .any(|(k, hdlr)| k == b"hdlr" && hdlr.get(8..12) == Some(b"vide".as_slice()))
        });
        if !is_video {
            return None;
        }
        let (_, tkhd) = children.iter().find(|(k, _)| k == b"tkhd")?;
        let tail = tkhd.get(tkhd.len().checked_sub(8)?..)?;
        let width = u32::from_be_bytes(tail[0..4].try_into().ok()?) >> 16;
        let height = u32::from_be_bytes(tail[4..8].try_into().ok()?) >> 16;
        (width > 0 && height > 0).then_some((width, height))
    })
}

/// Every chunk offset of every track (`moov/trak/mdia/minf/stbl/stco` or `co64`)
/// must point inside the file (`len` bytes): the samples live there, so an offset
/// at or past the end means the media data was cut off. Sample tables that cannot
/// be parsed are not judged here (the walk only reports what it can read).
fn check_chunk_offsets(moov: &[u8], len: u64) -> Result<(), String> {
    for (_, trak) in child_boxes(moov).into_iter().filter(|(kind, _)| kind == b"trak") {
        let tables = child_boxes(trak)
            .into_iter()
            .filter(|(kind, _)| kind == b"mdia")
            .flat_map(|(_, mdia)| child_boxes(mdia))
            .filter(|(kind, _)| kind == b"minf")
            .flat_map(|(_, minf)| child_boxes(minf))
            .filter(|(kind, _)| kind == b"stbl")
            .flat_map(|(_, stbl)| child_boxes(stbl));
        for (kind, body) in tables {
            let width = match &kind {
                b"stco" => 4,
                b"co64" => 8,
                _ => continue,
            };
            // version + flags, entry_count, then the offsets.
            let Some(count) = body.get(4..8).and_then(|b| <[u8; 4]>::try_from(b).ok()) else { continue };
            let count = usize::try_from(u32::from_be_bytes(count)).unwrap_or(usize::MAX);
            let entries = body.get(8..).unwrap_or_default();
            for entry in entries.chunks_exact(width).take(count) {
                let offset = match width {
                    4 => entry.try_into().map(|b| u64::from(u32::from_be_bytes(b))),
                    _ => entry.try_into().map(u64::from_be_bytes),
                };
                let Ok(offset) = offset else { continue };
                if offset >= len {
                    return Err(format!(
                        "the media data is incomplete: a sample chunk starts at offset {offset}, but the file \
                         has only {len} bytes (truncated file?)"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Duration from the `mvhd` child of a `moov` payload, if present and meaningful.
fn mvhd_duration(moov: &[u8]) -> Option<f64> {
    let (_, body) = child_boxes(moov).into_iter().find(|(kind, _)| kind == b"mvhd")?;
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
    // Fragmented MP4s leave the movie duration 0 (their length lives in the
    // fragments): that is "unknown", not a zero-second video.
    if timescale == 0 || duration == 0 {
        return None;
    }
    let seconds = duration as f64 / f64::from(timescale);
    Some((seconds * 1000.0).round() / 1000.0)
}
