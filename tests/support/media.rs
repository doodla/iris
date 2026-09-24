//! Media fixtures: small real images and a minimal valid MP4.

use std::io::Cursor;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

fn encode(img: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, format).unwrap();
    buf.into_inner()
}

/// An opaque RGBA PNG of `width`×`height` in `color`.
pub fn png_colored(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
    let [r, g, b] = color;
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([r, g, b, 255]));
    encode(image::DynamicImage::ImageRgba8(img), image::ImageFormat::Png)
}

/// An opaque RGBA PNG.
pub fn png(width: u32, height: u32) -> Vec<u8> {
    png_colored(width, height, [10, 120, 200])
}

/// A fully transparent RGBA PNG (an OpenAI edit mask: transparent = edit here).
pub fn mask_png(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([0, 0, 0, 0]));
    encode(image::DynamicImage::ImageRgba8(img), image::ImageFormat::Png)
}

/// A baseline JPEG.
pub fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30]));
    encode(image::DynamicImage::ImageRgb8(img), image::ImageFormat::Jpeg)
}

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A minimal valid MP4 (`ftyp`, `moov` with `mvhd`, `mdat`) lasting `seconds`.
pub fn mp4(seconds: u32) -> Vec<u8> {
    let mut ftyp = b"isom".to_vec();
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    for brand in [b"isom", b"iso2", b"mp41"] {
        ftyp.extend_from_slice(brand);
    }
    let mut mvhd = vec![0u8; 4];
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&(seconds * 1000).to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 80]);
    let trak = bx(b"trak", &bx(b"tkhd", &[0u8; 84]));
    let moov = bx(b"moov", &[bx(b"mvhd", &mvhd), trak].concat());
    [bx(b"ftyp", &ftyp), moov, bx(b"mdat", &[0xAB; 2048])].concat()
}

/// Standard base64.
pub fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// `data:<mime>;base64,<bytes>`.
pub fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", b64(bytes))
}
