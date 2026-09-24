//! Local input image validation before paid requests (SPEC §3, C-01 `InputImage`).

use std::fs;
use std::io::Cursor;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use iris::artifacts::read_input_image;
use iris::catalog::InputSpec;
use iris::error::ErrorCode;
use iris::providers::InputRole;

const SPEC: InputSpec = InputSpec {
    max_input_images: 4,
    input_media_types: &["image/png", "image/jpeg", "image/webp"],
    max_input_bytes: 50_000,
    mask: true,
    first_frame: false,
    last_frame: false,
    max_reference_images: 0,
};

fn png(width: u32, height: u32) -> Vec<u8> {
    let img = DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| Rgb([x as u8, y as u8, 7])));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png).unwrap();
    buf.into_inner()
}

#[test]
fn valid_image_is_read_with_sniffed_type_and_safe_upload_name() {
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
    assert_eq!(img.file_name, "my_photo__1_.png");
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
    assert_eq!(img.file_name, "photo.heic");
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
