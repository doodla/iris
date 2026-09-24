//! Reading and validating local input images before any paid request (see the
//! `artifacts` row in docs/architecture.md).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::catalog::{InputSpec, ResolvedOptions};
use crate::error::{ErrorCode, IrisError};
use crate::providers::{InputImage, InputRole};

use super::media;

/// Read `path` as an input image for `role`, validated against the model's
/// declared [`InputSpec`]:
///
/// * the file exists, is a regular file, and is readable;
/// * its size is at most `max_input_bytes` (a mask: the mask's `max_bytes`);
/// * its media type, sniffed from the content (never the extension), is in
///   `input_media_types` (a mask: the mask's `media_types`);
/// * the content is intact (corrupt or truncated files are caught locally): PNG,
///   JPEG, and WebP decode fully; GIF and HEIC/HEIF pass a structural walk;
/// * a mask that must have an alpha channel has one.
///
/// Every failure is `input_file_invalid` naming the file (and the accepted types
/// where relevant). The returned `path` is absolute; `file_name` is a sanitized
/// name (`[A-Za-z0-9._-]`) with the extension of the sniffed type. Rules that relate
/// several inputs are checked by [`check_request_inputs`].
pub fn read_input_image(path: &Path, role: InputRole, spec: &InputSpec) -> Result<InputImage, IrisError> {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let shown = abs.display().to_string();
    let invalid = |message: String| {
        IrisError::new(ErrorCode::InputFileInvalid, message).with_detail("path", shown.clone())
    };
    let label = role_label(role);
    let mask = if role == InputRole::Mask { spec.mask } else { None };
    let (media_types, max) = match mask {
        Some(m) => (m.media_types, m.max_bytes),
        None => (spec.input_media_types, spec.max_input_bytes),
    };

    if media_types.is_empty() || max == 0 {
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

    let accepted = media_types.join(", ");
    let media_type = media::sniff(&bytes).ok_or_else(|| {
        invalid(format!("{label} {shown} is not a recognized image format; this model accepts {accepted}"))
    })?;
    if !media::is_image(media_type) || !media::accepts(media_types, media_type) {
        return Err(invalid(format!("{label} {shown} is {media_type}; this model accepts {accepted}")));
    }
    media::validate_bytes(&bytes, &[])
        .map_err(|e| invalid(format!("{label} {shown} is corrupt or truncated: {}", e.message)))?;
    if mask.is_some_and(|m| m.requires_alpha) {
        let details = media::inspect_image(&bytes)
            .map_err(|e| invalid(format!("{label} {shown} cannot be decoded: {}", e.message)))?;
        if !details.has_alpha {
            return Err(invalid(format!(
                "{label} {shown} has no alpha channel; this model edits the areas where the mask is fully \
                 transparent, so the mask needs transparency (a PNG with an alpha channel)"
            )));
        }
    }

    Ok(InputImage {
        role,
        file_name: upload_name(&abs, media_type),
        path: abs,
        media_type: media_type.to_string(),
        bytes,
    })
}

/// Rules that relate several inputs of one request, checked after every input was
/// read and before a dry run returns or a credential is needed (the adapter keeps
/// its own checks as a second line of defense):
///
/// * a mask that must match the first `--image` has its pixel dimensions
///   (`input_file_invalid` naming the mask);
/// * a documented cap on the whole inline request is not exceeded, using the
///   catalog's upper bound of the encoded size (`invalid_argument` with
///   `details.request_bytes` and `details.limit_bytes`).
pub fn check_request_inputs<'a>(
    spec: &InputSpec,
    prompt: &str,
    options: &ResolvedOptions,
    inputs: impl IntoIterator<Item = &'a InputImage>,
) -> Result<(), IrisError> {
    let inputs: Vec<&InputImage> = inputs.into_iter().collect();
    if let Some(rules) = spec.mask
        && rules.same_size_as_first_image
        && let Some(mask) = inputs.iter().find(|i| i.role == InputRole::Mask)
        && let Some(first) = inputs.iter().find(|i| i.role == InputRole::Image)
    {
        check_same_size(mask, first)?;
    }
    if let Some(limit) = spec.max_request {
        let size = limit.upper_bound(prompt, options, inputs.iter().map(|i| i.bytes.len() as u64));
        if size > limit.max_bytes {
            return Err(IrisError::invalid(format!(
                "the request would be up to {size} bytes once encoded (the prompt, options, and base64 input \
                 images); this model accepts inline requests of at most {} bytes",
                limit.max_bytes
            ))
            .with_hint("use fewer or smaller input images (base64 adds about a third to each image)")
            .with_detail("request_bytes", size)
            .with_detail("limit_bytes", limit.max_bytes));
        }
    }
    Ok(())
}

/// The mask must have the pixel dimensions of the first input image.
fn check_same_size(mask: &InputImage, first: &InputImage) -> Result<(), IrisError> {
    let invalid = |image: &InputImage, message: String| {
        IrisError::new(ErrorCode::InputFileInvalid, message)
            .with_detail("path", image.path.display().to_string())
    };
    let dims = |image: &InputImage| media::inspect_image(&image.bytes).map(|d| (d.width, d.height));
    let (mw, mh) = dims(mask).map_err(|e| {
        invalid(
            mask,
            format!("cannot read the dimensions of mask image {}: {}", mask.path.display(), e.message),
        )
    })?;
    let (fw, fh) = dims(first).map_err(|e| {
        invalid(
            first,
            format!("cannot read the dimensions of input image {}: {}", first.path.display(), e.message),
        )
    })?;
    if (mw, mh) != (fw, fh) {
        return Err(invalid(
            mask,
            format!(
                "mask image {} is {mw}x{mh} but the first input image {} is {fw}x{fh}; the mask applies to the \
                 first image and must have the same dimensions",
                mask.path.display(),
                first.path.display()
            ),
        ));
    }
    Ok(())
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
