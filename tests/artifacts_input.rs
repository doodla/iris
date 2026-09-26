//! Local input image validation before paid requests (see the `artifacts` row in
//! docs/contributing/architecture.md).

use std::fs;
use std::io::Cursor;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use iris::artifacts::{check_request_inputs, read_input_image};
use iris::catalog::{InputSpec, MaskSpec, OptionValue, RequestSizeLimit, ResolvedOptions};
use iris::error::ErrorCode;
use iris::providers::{InputImage, InputRole};

const SPEC: InputSpec = InputSpec {
    max_input_images: 4,
    input_media_types: &["image/png", "image/jpeg", "image/webp"],
    max_input_bytes: 50_000,
    mask: Some(MaskSpec {
        media_types: &["image/png"],
        max_bytes: 20_000,
        requires_alpha: true,
        same_size_as_first_image: true,
    }),
    first_frame: false,
    last_frame: false,
    max_reference_images: 0,
    max_request: None,
};

fn png(width: u32, height: u32) -> Vec<u8> {
    let img = DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| Rgb([x as u8, y as u8, 7])));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png).unwrap();
    buf.into_inner()
}

#[test]
fn valid_image_is_read_with_its_sniffed_type() {
    let dir = tempfile::tempdir().unwrap();
    // PNG content behind a misleading name and extension.
    let path = dir.path().join("my photo (1).JPEG");
    let bytes = png(8, 8);
    fs::write(&path, &bytes).unwrap();
    let img = read_input_image(&path, InputRole::Image, &SPEC).unwrap();
    assert_eq!(img.role, InputRole::Image);
    assert_eq!(img.media_type, "image/png");
    assert_eq!(img.bytes, bytes);
    assert_eq!(img.path, path);
    assert!(img.path.is_absolute());
}

#[test]
fn missing_unreadable_and_directory_inputs_are_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.png");
    let err = read_input_image(&missing, InputRole::Image, &SPEC).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert_eq!(err.exit_code(), 2);
    assert!(err.message.contains("does not exist"));
    assert!(err.message.contains(missing.to_str().unwrap()));
    assert_eq!(err.details["path"], missing.to_str().unwrap());

    let err = read_input_image(dir.path(), InputRole::Reference, &SPEC).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("reference image"));
    assert!(err.message.contains("not a regular file"));
}

#[test]
fn oversized_inputs_are_rejected_before_reading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.png");
    let bytes = png(8, 8);
    fs::write(&path, &bytes).unwrap();
    let tiny = InputSpec { max_input_bytes: bytes.len() as u64 - 1, ..SPEC };
    let err = read_input_image(&path, InputRole::FirstFrame, &tiny).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains(&format!("{} bytes", bytes.len())), "{}", err.message);
    assert!(err.message.contains(&format!("at most {} bytes", bytes.len() - 1)), "{}", err.message);
    // Exactly at the limit is fine.
    let exact = InputSpec { max_input_bytes: bytes.len() as u64, ..SPEC };
    read_input_image(&path, InputRole::FirstFrame, &exact).unwrap();
}

#[test]
fn unaccepted_types_name_the_file_and_the_accepted_types() {
    let dir = tempfile::tempdir().unwrap();
    let gif = dir.path().join("anim.gif");
    fs::write(&gif, b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap();
    let err = read_input_image(&gif, InputRole::Image, &SPEC).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("anim.gif"));
    assert!(err.message.contains("image/gif"));
    assert!(err.message.contains("image/png, image/jpeg, image/webp"), "{}", err.message);

    let json = dir.path().join("fake.png");
    fs::write(&json, br#"{"not": "an image"}"#).unwrap();
    let err = read_input_image(&json, InputRole::Image, &SPEC).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("not a recognized image format"));

    let mp4 = dir.path().join("clip.png");
    fs::write(&mp4, [&[0, 0, 0, 16][..], b"ftypisom\0\0\0\0"].concat()).unwrap();
    let err = read_input_image(&mp4, InputRole::Image, &SPEC).unwrap_err();
    assert!(err.message.contains("video/mp4"), "{}", err.message);
}

#[test]
fn corrupt_images_are_caught_locally() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("half.png");
    let bytes = png(32, 32);
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    let err = read_input_image(&path, InputRole::Image, &SPEC).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("corrupt or truncated"), "{}", err.message);
}

/// ftyp(heic) + meta + mdat: the structure Iris checks for HEIC inputs.
fn heic() -> Vec<u8> {
    let bx = |kind: &[u8; 4], payload: &[u8]| {
        let mut out = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    };
    [bx(b"ftyp", b"heic\0\0\0\0mif1heic"), bx(b"meta", &[0u8; 32]), bx(b"mdat", &[0x33; 256])].concat()
}

#[test]
fn heic_is_accepted_by_sniffing_when_declared() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.heic");
    fs::write(&path, heic()).unwrap();
    let spec = InputSpec { input_media_types: &["image/png", "image/heif"], ..SPEC };
    let img = read_input_image(&path, InputRole::Image, &spec).unwrap();
    assert_eq!(img.media_type, "image/heic");
}

#[test]
fn truncated_heic_and_gif_inputs_are_caught_locally() {
    let dir = tempfile::tempdir().unwrap();
    let spec = InputSpec { input_media_types: &["image/heif", "image/gif"], ..SPEC };

    let path = dir.path().join("cut.heic");
    let full = heic();
    fs::write(&path, &full[..full.len() - 100]).unwrap();
    let err = read_input_image(&path, InputRole::Reference, &spec).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("cut.heic is corrupt or truncated"), "{}", err.message);

    let path = dir.path().join("cut.gif");
    fs::write(&path, b"GIF89a\x01\x00\x01\x00\x00\x00\x00").unwrap();
    let err = read_input_image(&path, InputRole::Image, &spec).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("corrupt or truncated"), "{}", err.message);
}

#[test]
fn models_without_image_inputs_reject_everything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    fs::write(&path, png(2, 2)).unwrap();
    let err = read_input_image(&path, InputRole::Image, &InputSpec::NONE).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("does not accept input images"));
}

fn encode(img: DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, format).unwrap();
    buf.into_inner()
}

/// A PNG with an alpha channel (an edit mask: transparent where the edit goes).
fn mask(width: u32, height: u32) -> Vec<u8> {
    encode(
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(width, height, image::Rgba([0, 0, 0, 0]))),
        ImageFormat::Png,
    )
}

fn read(
    dir: &std::path::Path,
    name: &str,
    bytes: &[u8],
    role: InputRole,
) -> Result<InputImage, iris::error::IrisError> {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    read_input_image(&path, role, &SPEC)
}

/// The mask rules the catalog declares (e.g. OpenAI's: PNG, alpha channel, size limit,
/// same dimensions as the first image) are enforced locally, before any request.
#[test]
fn masks_follow_the_declared_mask_rules() {
    let dir = tempfile::tempdir().unwrap();
    let ok = read(dir.path(), "m.png", &mask(8, 8), InputRole::Mask).unwrap();
    assert_eq!(ok.media_type, "image/png");

    // A JPEG is a fine input image but not an acceptable mask.
    let jpeg = encode(DynamicImage::ImageRgb8(RgbImage::new(8, 8)), ImageFormat::Jpeg);
    assert!(read(dir.path(), "i.jpg", &jpeg, InputRole::Image).is_ok());
    let err = read(dir.path(), "m.jpg", &jpeg, InputRole::Mask).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(
        err.message.contains("mask image") && err.message.contains("accepts image/png"),
        "{}",
        err.message
    );

    // No alpha channel.
    let err = read(dir.path(), "opaque.png", &png(8, 8), InputRole::Mask).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("no alpha channel"), "{}", err.message);
    assert!(err.details["path"].as_str().unwrap().ends_with("opaque.png"));

    // The mask's own size limit, below the input-image limit.
    let noisy = encode(
        DynamicImage::ImageRgba8(image::RgbaImage::from_fn(90, 90, |x, y| {
            let v = (x.wrapping_mul(7919) ^ y.wrapping_mul(104_729)).wrapping_mul(2_654_435_761);
            image::Rgba([v as u8, (v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8])
        })),
        ImageFormat::Png,
    );
    assert!(noisy.len() > 20_000 && noisy.len() <= 50_000, "{}", noisy.len());
    assert!(read(dir.path(), "big.png", &noisy, InputRole::Image).is_ok());
    let err = read(dir.path(), "big-mask.png", &noisy, InputRole::Mask).unwrap_err();
    assert!(err.message.contains("at most 20000 bytes"), "{}", err.message);

    // Same dimensions as the first input image.
    let first = read(dir.path(), "first.png", &png(8, 8), InputRole::Image).unwrap();
    let second = read(dir.path(), "second.png", &png(4, 4), InputRole::Image).unwrap();
    let small = read(dir.path(), "small.png", &mask(4, 4), InputRole::Mask).unwrap();
    let opts = ResolvedOptions::new();
    check_request_inputs(&SPEC, "p", &opts, [&first, &second, &ok]).unwrap();
    let err = check_request_inputs(&SPEC, "p", &opts, [&first, &second, &small]).unwrap_err();
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("4x4") && err.message.contains("8x8"), "{}", err.message);
    assert!(err.details["path"].as_str().unwrap().ends_with("small.png"));
    let relaxed =
        InputSpec { mask: SPEC.mask.map(|m| MaskSpec { same_size_as_first_image: false, ..m }), ..SPEC };
    check_request_inputs(&relaxed, "p", &opts, [&first, &small]).unwrap();
}

/// A declared cap on the whole inline request is checked with an upper bound of the
/// encoded size: prompt and option values as JSON strings, inputs as base64, plus
/// the declared framing allowances.
#[test]
fn inline_request_caps_use_an_upper_bound_of_the_encoded_size() {
    let dir = tempfile::tempdir().unwrap();
    let a = read(dir.path(), "a.png", &png(8, 8), InputRole::Image).unwrap();
    let b = read(dir.path(), "b.png", &png(16, 16), InputRole::Image).unwrap();
    let limit = RequestSizeLimit { max_bytes: 0, framing_bytes: 100, per_input_framing_bytes: 10 };
    let mut opts = ResolvedOptions::new();
    opts.insert("aspect_ratio", OptionValue::Str("16:9".into()));
    let prompt = "a \"quoted\"\nprompt";
    let b64 = |n: usize| n.div_ceil(3) * 4;
    let expected = 100
        + serde_json::to_string(prompt).unwrap().len()
        + "\"aspect_ratio\"".len()
        + "\"16:9\"".len()
        + b64(a.bytes.len())
        + 10
        + b64(b.bytes.len())
        + 10;
    let bound = limit.upper_bound(prompt, &opts, [a.bytes.len() as u64, b.bytes.len() as u64]);
    assert_eq!(bound, expected as u64);

    let at = InputSpec { max_request: Some(RequestSizeLimit { max_bytes: bound, ..limit }), ..SPEC };
    check_request_inputs(&at, prompt, &opts, [&a, &b]).unwrap();
    let below = InputSpec { max_request: Some(RequestSizeLimit { max_bytes: bound - 1, ..limit }), ..SPEC };
    let err = check_request_inputs(&below, prompt, &opts, [&a, &b]).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(err.details["request_bytes"], bound);
    assert_eq!(err.details["limit_bytes"], bound - 1);
    assert!(err.hint.as_deref().unwrap().contains("smaller input images"));
}
