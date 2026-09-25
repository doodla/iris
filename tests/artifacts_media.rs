//! Media sniffing, image decoding, and ISO-BMFF video validation (see docs/jobs.md).
//! Fixtures are generated in-test; nothing binary is committed.

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

/// A video track: tkhd (v0, 16.16 width/height at the end) + mdia/hdlr('vide').
fn video_trak(width: u32, height: u32) -> Vec<u8> {
    let mut tkhd = vec![0u8; 76]; // version/flags … matrix
    tkhd.extend_from_slice(&(width << 16).to_be_bytes());
    tkhd.extend_from_slice(&(height << 16).to_be_bytes());
    let mut hdlr = vec![0u8; 8]; // version/flags + pre_defined
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0u8; 13]); // reserved + empty name
    bx(b"trak", &[bx(b"tkhd", &tkhd), bx(b"mdia", &bx(b"hdlr", &hdlr))].concat())
}

/// A sound track whose tkhd carries no size (like real audio tracks).
fn sound_trak() -> Vec<u8> {
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(b"soun");
    hdlr.extend_from_slice(&[0u8; 13]);
    bx(b"trak", &[bx(b"tkhd", &[0u8; 84]), bx(b"mdia", &bx(b"hdlr", &hdlr))].concat())
}

#[test]
fn video_dimensions_come_from_the_video_track() {
    // Audio track first, then video: the size must come from the 'vide' track.
    let moov = bx(b"moov", &[mvhd_v0(1000, 4000), sound_trak(), video_trak(1280, 720)].concat());
    let mp4 = [ftyp(b"isom", &[b"isom"]), moov, bx(b"mdat", &[0u8; 64])].concat();
    let info = inspect_iso_bmff(&mut Cursor::new(&mp4)).unwrap();
    assert_eq!((info.width, info.height), (Some(1280), Some(720)));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mp4");
    std::fs::write(&path, &mp4).unwrap();
    let file_info = media::validate_file(&path, &["video/mp4"]).unwrap();
    assert_eq!((file_info.width, file_info.height), (Some(1280), Some(720)));
    assert_eq!(file_info.duration_seconds, Some(4.0));

    // A file without a video track handler reports no size rather than a wrong one.
    let no_video = minimal_mp4(1000, 4000);
    let info = inspect_iso_bmff(&mut Cursor::new(&no_video)).unwrap();
    assert_eq!((info.width, info.height), (None, None));
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
    let mov = [ftyp(b"qt  ", &[b"qt  "]), bx(b"moov", &mvhd_v0(1000, 1500)), bx(b"mdat", &[2u8; 8])].concat();
    let info = inspect_iso_bmff(&mut Cursor::new(&mov)).unwrap();
    assert_eq!(info.media_type, "video/quicktime");

    // Unknown duration (all ones) or zero timescale: valid, duration unknown.
    let unknown =
        [ftyp(b"isom", &[]), bx(b"moov", &mvhd_v0(1000, u32::MAX)), bx(b"mdat", &[2u8; 8])].concat();
    assert_eq!(inspect_iso_bmff(&mut Cursor::new(&unknown)).unwrap().duration_seconds, None);
    let no_mvhd = [ftyp(b"isom", &[]), bx(b"moov", &bx(b"trak", &[])), bx(b"mdat", &[2u8; 8])].concat();
    assert_eq!(inspect_iso_bmff(&mut Cursor::new(&no_mvhd)).unwrap().duration_seconds, None);
    // Fragmented MP4: movie duration 0, samples in moof/mdat fragments.
    let fragmented = [
        ftyp(b"iso6", &[b"iso6", b"dash"]),
        bx(b"moov", &mvhd_v0(1000, 0)),
        bx(b"moof", &[]),
        bx(b"mdat", &[1; 8]),
    ]
    .concat();
    assert_eq!(inspect_iso_bmff(&mut Cursor::new(&fragmented)).unwrap().duration_seconds, None);
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

/// GIF with a global color table, a graphic control extension, a comment
/// extension, and one 3x2 image; `cut` bytes are removed from the end.
fn gif(cut: usize) -> Vec<u8> {
    let mut g = b"GIF89a".to_vec();
    g.extend_from_slice(&[3, 0, 2, 0, 0x80, 0, 0]); // 3x2, GCT of 2 entries
    g.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    g.extend_from_slice(&[0x21, 0xF9, 4, 0, 0, 0, 0, 0]); // graphic control extension
    g.extend_from_slice(&[0x21, 0xFE, 3, b'h', b'i', b'!', 0]); // comment extension
    g.extend_from_slice(&[0x2C, 0, 0, 0, 0, 3, 0, 2, 0, 0]); // image descriptor
    g.extend_from_slice(&[2, 3, 0x84, 0x1D, 0x05, 0]); // LZW min code size + data
    g.push(0x3B);
    g.truncate(g.len() - cut);
    g
}

fn heic(boxes: &[Vec<u8>]) -> Vec<u8> {
    [&[ftyp(b"heic", &[b"mif1", b"heic"])], boxes].concat().concat()
}

#[test]
fn gif_structure_is_walked_to_the_trailer() {
    let info = media::validate_bytes(&gif(0), &["image/gif"]).unwrap();
    assert_eq!(info.media_type, "image/gif");
    assert_eq!((info.width, info.height), (Some(3), Some(2)));
    assert_eq!(media::inspect_gif(&gif(0)), Ok((3, 2)));

    // Cut anywhere: missing trailer, inside the data sub-blocks, inside the
    // descriptor, inside the color table.
    for cut in [1, 3, 8, 20, 30] {
        let bytes = gif(cut);
        let err = media::validate_bytes(&bytes, &[]).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidMedia, "cut {cut}");
        assert!(err.message.contains("not a valid image/gif image"), "{}", err.message);
    }
    // Header and screen only: no image.
    let empty = [&gif(0)[..19], &[0x3B]].concat();
    assert!(media::inspect_gif(&empty).unwrap_err().contains("no image"));
    // An unknown block type.
    let mut odd = gif(1);
    odd.extend_from_slice(&[0x99, 0x3B]);
    assert!(media::inspect_gif(&odd).unwrap_err().contains("unexpected block 0x99"));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("anim.gif");
    std::fs::write(&path, gif(2)).unwrap();
    assert_eq!(media::validate_file(&path, &[]).unwrap_err().code, ErrorCode::InvalidMedia);
}

#[test]
fn heic_structure_is_walked_and_needs_image_items() {
    let meta = bx(b"meta", &[0u8; 32]);
    let good = heic(&[meta.clone(), bx(b"mdat", &[0x11; 128])]);
    let info = media::validate_bytes(&good, &["image/heif"]).unwrap();
    assert_eq!(info.media_type, "image/heic");
    assert_eq!((info.width, info.height), (None, None));
    let walked = media::inspect_heif(&mut Cursor::new(&good)).unwrap();
    assert_eq!(walked.major_brand, "heic");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.heic");
    std::fs::write(&path, &good).unwrap();
    assert_eq!(media::validate_file(&path, &[]).unwrap().media_type, "image/heic");

    let truncated = &good[..good.len() - 30];
    let err = media::validate_bytes(truncated, &[]).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    assert!(err.message.contains("not a valid image/heic image"), "{}", err.message);
    assert!(err.message.contains("truncated"), "{}", err.message);

    let no_items = heic(&[bx(b"mdat", &[0x11; 128])]);
    assert!(media::inspect_heif(&mut Cursor::new(&no_items)).unwrap_err().contains("'meta'"));
    let only_ftyp = heic(&[]);
    assert_eq!(media::validate_bytes(&only_ftyp, &[]).unwrap_err().code, ErrorCode::InvalidMedia);

    // Plain HEIF brand, and a video brand refused by the image walker.
    let heif = [ftyp(b"mif1", &[b"mif1"]), meta].concat();
    assert_eq!(media::validate_bytes(&heif, &[]).unwrap().media_type, "image/heif");
    let video = minimal_mp4(1000, 1000);
    assert!(
        media::inspect_heif(&mut Cursor::new(&video)).unwrap_err().contains("not a HEIC/HEIF image brand")
    );
}

/// A box header with a 64-bit `largesize` of `size` (whatever the payload).
fn crafted_large_bx(kind: &[u8; 4], size: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = 1u32.to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

#[test]
fn crafted_box_sizes_never_overflow_panic_or_loop() {
    // Sizes whose sum with any offset overflows u64 (and usize), placed where the
    // walker reads them: at top level, and inside moov, trak, and mdia (also as the
    // mvhd, tkhd, and hdlr boxes themselves, which are sliced when found).
    let huge = [u64::MAX, u64::MAX - 7, u64::MAX - 15, 1 << 63];
    for size in huge {
        let top = [ftyp(b"isom", &[]), crafted_large_bx(b"free", size, &[0u8; 16])].concat();
        let why = inspect_iso_bmff(&mut Cursor::new(&top)).unwrap_err();
        assert!(why.contains("needs") && why.contains("remain"), "{size}: {why}");
        assert_eq!(media::validate_bytes(&top, &[]).unwrap_err().code, ErrorCode::InvalidMedia);

        for kind in [b"free", b"mvhd", b"trak"] {
            let moov = bx(b"moov", &[bx(b"free", &[]), crafted_large_bx(kind, size, &[0u8; 32])].concat());
            let file = [ftyp(b"isom", &[]), moov, bx(b"mdat", &[1u8; 16])].concat();
            let info = inspect_iso_bmff(&mut Cursor::new(&file)).unwrap();
            assert_eq!(info.duration_seconds, None, "{size} {kind:?}");
            assert_eq!((info.width, info.height), (None, None));
        }

        // Inside a trak (next to its tkhd) and inside its mdia (next to the hdlr).
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(b"vide");
        hdlr.extend_from_slice(&[0u8; 13]);
        let mut tkhd = vec![0u8; 76];
        tkhd.extend_from_slice(&(640u32 << 16).to_be_bytes());
        tkhd.extend_from_slice(&(360u32 << 16).to_be_bytes());
        let traks = [
            bx(b"trak", &[crafted_large_bx(b"tkhd", size, &tkhd)].concat()),
            bx(b"trak", &[bx(b"tkhd", &tkhd), crafted_large_bx(b"mdia", size, &bx(b"hdlr", &hdlr))].concat()),
            bx(b"trak", &[bx(b"tkhd", &tkhd), bx(b"mdia", &crafted_large_bx(b"hdlr", size, &hdlr))].concat()),
        ];
        for trak in traks {
            let moov = bx(b"moov", &[mvhd_v0(1000, 4000), trak].concat());
            let file = [ftyp(b"isom", &[]), moov, bx(b"mdat", &[1u8; 16])].concat();
            let info = inspect_iso_bmff(&mut Cursor::new(&file)).unwrap();
            assert_eq!(info.duration_seconds, Some(4.0), "the mvhd before the crafted box still counts");
            assert_eq!((info.width, info.height), (None, None), "no size is read from a malformed track");
            // The same through the file-based path the downloader uses.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("crafted.mp4");
            std::fs::write(&path, &file).unwrap();
            assert_eq!(media::validate_file(&path, &["video/mp4"]).unwrap().duration_seconds, Some(4.0));
        }
    }

    // A well-formed 64-bit child header is still understood.
    let moov = bx(b"moov", &large_bx(b"mvhd", &mvhd_v0(1000, 3000)[8..]));
    let file = [ftyp(b"isom", &[]), moov, bx(b"mdat", &[1u8; 16])].concat();
    assert_eq!(inspect_iso_bmff(&mut Cursor::new(&file)).unwrap().duration_seconds, Some(3.0));
}

/// A track whose sample table places its chunks at `offsets` (`stco`, or `co64`
/// when `wide`).
fn trak_with_chunks(offsets: &[u64], wide: bool) -> Vec<u8> {
    let mut table = vec![0u8; 4]; // version + flags
    table.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
    for &offset in offsets {
        if wide {
            table.extend_from_slice(&offset.to_be_bytes());
        } else {
            table.extend_from_slice(&(offset as u32).to_be_bytes());
        }
    }
    let chunk_box = bx(if wide { b"co64" } else { b"stco" }, &table);
    let stbl = bx(b"stbl", &[bx(b"stsd", &[0u8; 8]), chunk_box].concat());
    bx(b"trak", &[bx(b"tkhd", &[0u8; 84]), bx(b"mdia", &bx(b"minf", &stbl))].concat())
}

/// ftyp + moov (one track whose chunks start at the given offsets into the file)
/// + a 256-byte mdat. Returns the file and the offset of the mdat payload.
fn mp4_with_chunks(chunks_in_mdat: &[u64], wide: bool) -> (Vec<u8>, u64) {
    let head = ftyp(b"isom", &[b"isom"]);
    // The moov size does not depend on the offset values, so lay it out once to
    // learn where the media data starts.
    let probe = bx(b"moov", &[mvhd_v0(1000, 2000), trak_with_chunks(chunks_in_mdat, wide)].concat());
    let data_start = (head.len() + probe.len() + 8) as u64;
    let absolute: Vec<u64> = chunks_in_mdat.iter().map(|o| data_start + o).collect();
    let moov = bx(b"moov", &[mvhd_v0(1000, 2000), trak_with_chunks(&absolute, wide)].concat());
    ([head, moov, bx(b"mdat", &[0x33; 256])].concat(), data_start)
}

#[test]
fn videos_need_media_data_that_the_sample_tables_can_reach() {
    // Chunk offsets inside the media data: valid, with stco and with co64.
    for wide in [false, true] {
        let (file, _) = mp4_with_chunks(&[0, 100, 200], wide);
        let info = inspect_iso_bmff(&mut Cursor::new(&file)).unwrap();
        assert_eq!(info.duration_seconds, Some(2.0));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.mp4");
        std::fs::write(&path, &file).unwrap();
        assert_eq!(media::validate_file(&path, &["video/mp4"]).unwrap().media_type, "video/mp4");
    }

    // Cut right after the metadata (the case a file host that stops early
    // produces): no media data at all.
    let (file, data_start) = mp4_with_chunks(&[0, 100, 200], false);
    let metadata_only = &file[..(data_start - 8) as usize];
    let why = inspect_iso_bmff(&mut Cursor::new(metadata_only)).unwrap_err();
    assert!(why.contains("sample chunk") && why.contains("truncated"), "{why}");
    let err = media::validate_bytes(metadata_only, &["video/mp4"]).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidMedia);
    // Without sample tables to go by, the missing media data box itself.
    let no_mdat = [ftyp(b"isom", &[]), bx(b"moov", &mvhd_v0(1000, 4000))].concat();
    let why = inspect_iso_bmff(&mut Cursor::new(&no_mdat)).unwrap_err();
    assert!(why.contains("'mdat'") && why.contains("truncated"), "{why}");

    // A media data box that claims to extend to the end of the file (size 0), cut
    // before the last chunk: the sample table reaches past the end.
    for wide in [false, true] {
        let (mut file, data_start) = mp4_with_chunks(&[0, 100, 200], wide);
        let at = (data_start - 8) as usize;
        file[at..at + 4].copy_from_slice(&0u32.to_be_bytes());
        assert!(inspect_iso_bmff(&mut Cursor::new(&file)).is_ok(), "complete: every chunk is inside");
        file.truncate((data_start + 150) as usize);
        let why = inspect_iso_bmff(&mut Cursor::new(&file)).unwrap_err();
        assert!(why.contains("sample chunk") && why.contains("truncated"), "{why}");
    }

    // A chunk offset far past the end (e.g. a 64-bit offset near u64::MAX).
    let (file, _) = mp4_with_chunks(&[0, u64::MAX / 2], true);
    assert!(inspect_iso_bmff(&mut Cursor::new(&file)).unwrap_err().contains("sample chunk"));

    // An entry count larger than the table holds is read as far as the table goes.
    let mut table = vec![0u8; 4];
    table.extend_from_slice(&u32::MAX.to_be_bytes());
    table.extend_from_slice(&40u32.to_be_bytes());
    let stbl = bx(b"stbl", &bx(b"stco", &table));
    let trak = bx(b"trak", &bx(b"mdia", &bx(b"minf", &stbl)));
    let file = [ftyp(b"isom", &[]), bx(b"moov", &trak), bx(b"mdat", &[1u8; 64])].concat();
    assert!(inspect_iso_bmff(&mut Cursor::new(&file)).is_ok());
}
