//! Media sniffing, image decoding, and ISO-BMFF video validation (C-04 "Artifact
//! validation"). Fixtures are generated in-test; nothing binary is committed.

use std::io::Cursor;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use iris::artifacts::media::{self, inspect_iso_bmff};
use iris::error::ErrorCode;

fn encode(format: ImageFormat, width: u32, height: u32, alpha: bool) -> Vec<u8> {
    let img = if alpha {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, Rgba([200, 30, 30, 100])))
    } else {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, Rgb([30, 200, 30])))
    };
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, format).unwrap();
    buf.into_inner()
}

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A box with a 64-bit `largesize` header.
fn large_bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = 1u32.to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(&((16 + payload.len()) as u64).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
    let mut payload = major.to_vec();
    payload.extend_from_slice(&0u32.to_be_bytes());
    for c in compatible {
        payload.extend_from_slice(*c);
    }
    bx(b"ftyp", &payload)
}

fn mvhd_v0(timescale: u32, duration: u32) -> Vec<u8> {
    let mut p = vec![0u8; 4]; // version 0 + flags
    p.extend_from_slice(&0u32.to_be_bytes()); // creation
    p.extend_from_slice(&0u32.to_be_bytes()); // modification
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&[0u8; 80]); // rate, volume, reserved, matrix, pre_defined, next_track_ID
    bx(b"mvhd", &p)
}

fn mvhd_v1(timescale: u32, duration: u64) -> Vec<u8> {
    let mut p = vec![1u8, 0, 0, 0]; // version 1 + flags
    p.extend_from_slice(&0u64.to_be_bytes());
    p.extend_from_slice(&0u64.to_be_bytes());
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&[0u8; 80]);
    bx(b"mvhd", &p)
}

/// ftyp + moov(mvhd) + mdat: the minimal shape of a playable MP4 as far as Iris checks.
fn minimal_mp4(timescale: u32, duration: u32) -> Vec<u8> {
    let trak = bx(b"trak", &bx(b"tkhd", &[0u8; 84]));
    let moov = bx(b"moov", &[mvhd_v0(timescale, duration), trak].concat());
    [ftyp(b"isom", &[b"isom", b"iso2", b"mp41"]), moov, bx(b"mdat", &[0xAB; 256])].concat()
}

#[test]
fn sniffs_images_from_magic_bytes() {
    assert_eq!(media::sniff(&encode(ImageFormat::Png, 2, 2, false)), Some("image/png"));
    assert_eq!(media::sniff(&encode(ImageFormat::Jpeg, 2, 2, false)), Some("image/jpeg"));
    assert_eq!(media::sniff(&encode(ImageFormat::WebP, 2, 2, true)), Some("image/webp"));
    assert_eq!(media::sniff(b"GIF89a\x01\x00\x01\x00\x00\x00\x00"), Some("image/gif"));
    assert_eq!(media::sniff(b"{\"error\": {\"code\": 403}}"), None);
    assert_eq!(media::sniff(b"<html><body>Forbidden</body></html>"), None);
    assert_eq!(media::sniff(b""), None);
    // "WEBP" at offset 8 without a RIFF header is not WebP.
    assert_eq!(media::sniff(b"XXXXXXXXWEBPVP8 "), None);
}

#[test]
fn sniffs_iso_bmff_by_brand() {
    assert_eq!(media::sniff(&minimal_mp4(1000, 4000)), Some("video/mp4"));
    assert_eq!(media::sniff(&ftyp(b"qt  ", &[b"qt  "])), Some("video/quicktime"));
    assert_eq!(media::sniff(&ftyp(b"mp42", &[b"isom"])), Some("video/mp4"));
    assert_eq!(media::sniff(&ftyp(b"iso8", &[b"iso8"])), Some("video/mp4"));
    assert_eq!(media::sniff(&ftyp(b"heic", &[b"mif1", b"heic"])), Some("image/heic"));
    assert_eq!(media::sniff(&ftyp(b"mif1", &[b"mif1", b"heic"])), Some("image/heic"));
    assert_eq!(media::sniff(&ftyp(b"mif1", &[b"mif1"])), Some("image/heif"));
    assert_eq!(media::sniff(&ftyp(b"avif", &[b"mif1", b"avif"])), None);
    assert_eq!(media::sniff(&ftyp(b"M4A ", &[b"M4A "])), None);
}

#[test]
fn extension_and_type_helpers() {
    assert_eq!(media::extension_for("image/jpeg"), Some("jpg"));
    assert_eq!(media::extension_for("image/jpg"), Some("jpg"));
    assert_eq!(media::extension_for("video/mp4"), Some("mp4"));
    assert_eq!(media::extension_for("video/quicktime"), Some("mov"));
    assert_eq!(media::extension_for("IMAGE/PNG; charset=binary"), Some("png"));
    assert_eq!(media::extension_for("application/json"), None);
    assert_eq!(media::media_type_for_extension("JPEG"), Some("image/jpeg"));
    assert_eq!(media::media_type_for_extension("txt"), None);
    assert!(media::accepts(&["image/png", "image/jpeg"], "image/jpeg"));
    assert!(media::accepts(&["image/jpg"], "image/jpeg"));
    assert!(media::accepts(&["image/heif"], "image/heic"));
    assert!(!media::accepts(&["image/png"], "image/webp"));
    assert!(media::accepts(&[], "video/mp4"));
}

#[test]
fn decodes_png_jpeg_and_webp_with_dimensions() {
    for (format, mt) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::WebP, "image/webp"),
    ] {
        let bytes = encode(format, 7, 5, false);
        let info = media::validate_bytes(&bytes, &[]).unwrap();
        assert_eq!(info.media_type, mt);
        assert_eq!((info.width, info.height), (Some(7), Some(5)));
        assert_eq!(info.duration_seconds, None);
    }
}

#[test]
fn truncated_images_fail_full_decode() {
    let png = encode(ImageFormat::Png, 64, 64, false);
    let err = media::validate_bytes(&png[..png.len() / 2], &["image/png"]).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("not a decodable image/png"), "{}", err.message);
}

#[test]
fn error_bodies_are_not_media() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("output.png");
    std::fs::write(&json, br#"{"error":{"code":403,"message":"Permission denied"}}"#).unwrap();
    let err = media::validate_file(&json, &["image/png"]).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("JSON"), "{}", err.message);
    assert!(!err.message.contains("Permission denied"), "content must not be echoed");

    let html = dir.path().join("video.mp4");
    std::fs::write(&html, b"\n  <!DOCTYPE html><html>Error 404</html>").unwrap();
    let err = media::validate_file(&html, &["video/mp4"]).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("HTML"), "{}", err.message);

    let empty = dir.path().join("empty.mp4");
    std::fs::write(&empty, b"").unwrap();
    assert!(media::validate_file(&empty, &[]).unwrap_err().message.contains("empty"));
}

#[test]
fn unexpected_media_type_is_rejected() {
    let jpeg = encode(ImageFormat::Jpeg, 2, 2, false);
    let err = media::validate_bytes(&jpeg, &["video/mp4"]).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("image/jpeg"));
    let heic = ftyp(b"heic", &[b"mif1", b"heic"]);
    assert_eq!(media::validate_bytes(&heic, &["video/mp4"]).unwrap_err().code, ErrorCode::InvalidMedia);
}

#[test]
fn png_alpha_and_dimensions() {
    let with_alpha = media::png_info(&encode(ImageFormat::Png, 4, 3, true)).unwrap();
    assert_eq!((with_alpha.width, with_alpha.height, with_alpha.has_alpha), (4, 3, true));
    let opaque = media::png_info(&encode(ImageFormat::Png, 4, 3, false)).unwrap();
    assert!(!opaque.has_alpha);
    let err = media::png_info(&encode(ImageFormat::Jpeg, 4, 3, false)).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("not image/png"));
}

#[test]
fn minimal_mp4_is_valid_with_duration() {
    let mp4 = minimal_mp4(1000, 4000);
    let info = inspect_iso_bmff(&mut Cursor::new(&mp4)).unwrap();
    assert_eq!(info.media_type, "video/mp4");
    assert_eq!(info.major_brand, "isom");
    assert_eq!(info.duration_seconds, Some(4.0));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mp4");
    std::fs::write(&path, &mp4).unwrap();
    let file_info = media::validate_file(&path, &["video/mp4"]).unwrap();
    assert_eq!(file_info.media_type, "video/mp4");
    assert_eq!(file_info.duration_seconds, Some(4.0));
    assert_eq!(media::validate_bytes(&mp4, &[]).unwrap(), file_info);
}

#[test]
fn iso_bmff_variants_are_accepted() {
    // moov at the end, 64-bit mdat header, mvhd v1, fractional duration.
    let moov = bx(b"moov", &mvhd_v1(90_000, 90_000 * 8 + 45_000));
    let file = [ftyp(b"mp42", &[b"isom"]), large_bx(b"mdat", &[1u8; 100]), bx(b"free", &[]), moov].concat();
    let info = inspect_iso_bmff(&mut Cursor::new(&file)).unwrap();
    assert_eq!(info.duration_seconds, Some(8.5));

    // Last box with size 0 extends to the end of the file.
    let mut open_ended = [ftyp(b"isom", &[]), bx(b"moov", &mvhd_v0(600, 1200))].concat();
    open_ended.extend_from_slice(&0u32.to_be_bytes());
    open_ended.extend_from_slice(b"mdat");
    open_ended.extend_from_slice(&[7u8; 50]);
    assert_eq!(inspect_iso_bmff(&mut Cursor::new(&open_ended)).unwrap().duration_seconds, Some(2.0));

    // QuickTime brand.
    let mov = [ftyp(b"qt  ", &[b"qt  "]), bx(b"moov", &mvhd_v0(1000, 1500))].concat();
    let info = inspect_iso_bmff(&mut Cursor::new(&mov)).unwrap();
    assert_eq!(info.media_type, "video/quicktime");

    // Unknown duration (all ones) or zero timescale: valid, duration unknown.
    let unknown = [ftyp(b"isom", &[]), bx(b"moov", &mvhd_v0(1000, u32::MAX))].concat();
    assert_eq!(inspect_iso_bmff(&mut Cursor::new(&unknown)).unwrap().duration_seconds, None);
    let no_mvhd = [ftyp(b"isom", &[]), bx(b"moov", &bx(b"trak", &[]))].concat();
    assert_eq!(inspect_iso_bmff(&mut Cursor::new(&no_mvhd)).unwrap().duration_seconds, None);
}

#[test]
fn broken_iso_bmff_is_rejected() {
    let mp4 = minimal_mp4(1000, 4000);

    let truncated = &mp4[..mp4.len() - 10];
    let why = inspect_iso_bmff(&mut Cursor::new(truncated)).unwrap_err();
    assert!(why.contains("truncated"), "{why}");
    let err = media::validate_bytes(truncated, &["video/mp4"]).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("not a valid video/mp4 video"), "{}", err.message);

    let no_moov = [ftyp(b"isom", &[]), bx(b"mdat", &[0u8; 32])].concat();
    assert!(inspect_iso_bmff(&mut Cursor::new(&no_moov)).unwrap_err().contains("moov"));

    let not_first = [bx(b"free", &[]), ftyp(b"isom", &[]), bx(b"moov", &mvhd_v0(1, 1))].concat();
    assert!(inspect_iso_bmff(&mut Cursor::new(&not_first)).unwrap_err().contains("not 'ftyp'"));

    let mut garbage_tail = mp4.clone();
    garbage_tail.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
    assert!(inspect_iso_bmff(&mut Cursor::new(&garbage_tail)).unwrap_err().contains("truncated box header"));

    let mut bad_size = [ftyp(b"isom", &[]), bx(b"moov", &mvhd_v0(1, 1))].concat();
    bad_size.extend_from_slice(&4u32.to_be_bytes()); // size smaller than its header
    bad_size.extend_from_slice(b"free");
    assert!(inspect_iso_bmff(&mut Cursor::new(&bad_size)).unwrap_err().contains("invalid size"));

    let mut binary_type = [ftyp(b"isom", &[]), bx(b"moov", &mvhd_v0(1, 1))].concat();
    binary_type.extend_from_slice(&8u32.to_be_bytes());
    binary_type.extend_from_slice(&[0, 1, 2, 3]);
    assert!(inspect_iso_bmff(&mut Cursor::new(&binary_type)).unwrap_err().contains("invalid box type"));

    let image_brand = [ftyp(b"heic", &[b"mif1"]), bx(b"moov", &mvhd_v0(1, 1))].concat();
    assert!(inspect_iso_bmff(&mut Cursor::new(&image_brand)).unwrap_err().contains("not a video brand"));

    assert!(inspect_iso_bmff(&mut Cursor::new(&[] as &[u8])).unwrap_err().contains("empty"));
}

#[test]
fn sniff_only_types_validate_without_dimensions() {
    let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;";
    let info = media::validate_bytes(gif, &["image/gif"]).unwrap();
    assert_eq!(info.media_type, "image/gif");
    assert_eq!(info.width, None);
}
