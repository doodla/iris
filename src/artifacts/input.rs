//! Reading and validating local input images before any paid request (see the
//! `artifacts` row in docs/architecture.md).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::catalog::InputSpec;
use crate::error::{ErrorCode, IrisError};
use crate::providers::{InputImage, InputRole};

use super::media;

/// Read `path` as an input image for `role`, validated against the model's
/// declared [`InputSpec`]:
///
/// * the file exists, is a regular file, and is readable;
/// * its size is at most `max_input_bytes`;
/// * its media type, sniffed from the content (never the extension), is in
///   `input_media_types`;
/// * the content is intact (corrupt or truncated files are caught locally): PNG,
///   JPEG, and WebP decode fully; GIF and HEIC/HEIF pass a structural walk.
///
/// Every failure is `input_file_invalid` naming the file (and the accepted types
/// where relevant). The returned `path` is absolute; `file_name` is a sanitized
/// name (`[A-Za-z0-9._-]`) with the extension of the sniffed type.
pub fn read_input_image(path: &Path, role: InputRole, spec: &InputSpec) -> Result<InputImage, IrisError> {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let shown = abs.display().to_string();
    let invalid = |message: String| {
        IrisError::new(ErrorCode::InputFileInvalid, message).with_detail("path", shown.clone())
    };
    let label = role_label(role);

    if spec.input_media_types.is_empty() || spec.max_input_bytes == 0 {
        return Err(invalid(format!("{label} {shown}: this model does not accept input images")));
    }

    let meta = std::fs::metadata(&abs).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => invalid(format!("{label} {shown} does not exist")),
        std::io::ErrorKind::PermissionDenied => invalid(format!("{label} {shown} is not readable")),
        _ => invalid(format!("{label} {shown} cannot be read: {e}")),
    })?;
    if !meta.is_file() {
        return Err(invalid(format!("{label} {shown} is not a regular file")));
    }
    let max = spec.max_input_bytes;
    if meta.len() > max {
        return Err(too_large(invalid, &label, &shown, meta.len(), max));
    }

    let file = File::open(&abs).map_err(|e| invalid(format!("{label} {shown} cannot be opened: {e}")))?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    // Read at most max+1 bytes so a file that grows after the size check is still caught.
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| invalid(format!("{label} {shown} cannot be read: {e}")))?;
    if bytes.len() as u64 > max {
        return Err(too_large(invalid, &label, &shown, bytes.len() as u64, max));
    }

    let accepted = spec.input_media_types.join(", ");
    let media_type = media::sniff(&bytes).ok_or_else(|| {
        invalid(format!("{label} {shown} is not a recognized image format; this model accepts {accepted}"))
    })?;
    if !media::is_image(media_type) || !media::accepts(spec.input_media_types, media_type) {
        return Err(invalid(format!("{label} {shown} is {media_type}; this model accepts {accepted}")));
    }
    media::validate_bytes(&bytes, &[])
        .map_err(|e| invalid(format!("{label} {shown} is corrupt or truncated: {}", e.message)))?;

    Ok(InputImage {
        role,
        file_name: upload_name(&abs, media_type),
        path: abs,
        media_type: media_type.to_string(),
        bytes,
    })
}

fn too_large(
    invalid: impl Fn(String) -> IrisError,
    label: &str,
    shown: &str,
    size: u64,
    max: u64,
) -> IrisError {
    invalid(format!(
        "{label} {shown} is {size} bytes; this model accepts at most {max} bytes per input image"
    ))
}

fn role_label(role: InputRole) -> String {
    match role {
        InputRole::Image => "input image",
        InputRole::Mask => "mask image",
        InputRole::FirstFrame => "first-frame image",
        InputRole::LastFrame => "last-frame image",
        InputRole::Reference => "reference image",
    }
    .to_string()
}

/// `<sanitized stem>.<canonical ext>`, for multipart uploads and provider display.
fn upload_name(path: &Path, media_type: &str) -> String {
    let stem: String = path
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .take(100)
        .collect();
    let stem = if stem.trim_matches('.').is_empty() { "image".to_string() } else { stem };
    let ext = media::extension_for(media_type).unwrap_or("bin");
    format!("{stem}.{ext}")
}
