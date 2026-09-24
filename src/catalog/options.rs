//! Option values and their validation against declared [`OptionSpec`]s.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use super::types::{ModelSpec, OptionKind, ValidationInput};
use crate::domain::Operation;
use crate::error::{ErrorCode, IrisError};

/// A typed, validated option value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum OptionValue {
    Str(String),
    Int(i64),
    Bool(bool),
}

impl OptionValue {
    /// Parse and validate `raw` against `kind`. The error string lists what is accepted.
    pub fn parse(kind: &OptionKind, raw: &str) -> Result<OptionValue, String> {
        match kind {
            OptionKind::Enum(values) => {
                if values.contains(&raw) {
                    Ok(OptionValue::Str(raw.to_string()))
                } else {
                    Err(format!("expected one of: {}", values.join(", ")))
                }
            }
            OptionKind::Integer { min, max } => match raw.trim().parse::<i64>() {
                Ok(n) if n >= *min && n <= *max => Ok(OptionValue::Int(n)),
                _ => Err(format!("expected an integer from {min} to {max}")),
            },
            OptionKind::Boolean => match raw {
                "true" => Ok(OptionValue::Bool(true)),
                "false" => Ok(OptionValue::Bool(false)),
                _ => Err("expected true or false".to_string()),
            },
            OptionKind::Text { max_chars } => {
                if raw.chars().count() <= *max_chars {
                    Ok(OptionValue::Str(raw.to_string()))
                } else {
                    Err(format!("expected at most {max_chars} characters"))
                }
            }
            OptionKind::Pattern { syntax, validate } => match validate(raw) {
                Ok(()) => Ok(OptionValue::Str(raw.to_string())),
                Err(why) => Err(format!("{why} (expected {syntax})")),
            },
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            OptionValue::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            OptionValue::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            OptionValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

impl fmt::Display for OptionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OptionValue::Str(s) => f.write_str(s),
            OptionValue::Int(n) => write!(f, "{n}"),
            OptionValue::Bool(b) => write!(f, "{b}"),
        }
    }
}

/// Options the user set explicitly, validated against the model. Omitted options are
/// not present (adapters must not send them; the provider default applies).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ResolvedOptions(BTreeMap<String, OptionValue>);

impl ResolvedOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, name: &str) -> Option<&OptionValue> {
        self.0.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    pub fn insert(&mut self, name: impl Into<String>, value: OptionValue) {
        self.0.insert(name.into(), value);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &OptionValue)> {
        self.0.iter()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Where a raw option came from, for precise error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionSource {
    /// A typed CLI flag such as `--quality`.
    Flag(&'static str),
    /// `-O name=value`.
    Generic,
}

/// An option as given by the user, before validation.
#[derive(Debug, Clone)]
pub struct RawOption {
    pub name: String,
    pub value: String,
    pub source: OptionSource,
}

/// Input counts by role, for capability validation.
#[derive(Debug, Clone, Copy, Default)]
pub struct InputCounts {
    pub images: usize,
    pub mask: bool,
    pub first_frame: bool,
    pub last_frame: bool,
    pub references: usize,
}

/// Validate a request's operation, options, and input counts against `spec`.
///
/// Returns the explicitly-set options, typed. Never drops anything: every raw option
/// is either accepted or produces an error.
pub fn validate_request(
    spec: &ModelSpec,
    operation: Operation,
    raw: &[RawOption],
    inputs: InputCounts,
) -> Result<ResolvedOptions, IrisError> {
    if !spec.supports(operation) {
        let supported: Vec<&str> = spec.operations.iter().map(|o| o.as_str()).collect();
        return Err(IrisError::new(
            ErrorCode::UnsupportedOperation,
            format!("model '{}' does not support {operation} (supports: {})", spec.id, supported.join(", ")),
        )
        .with_hint("run `iris models list` to see which models support this operation"));
    }

    let mut resolved = ResolvedOptions::new();
    for opt in raw {
        let describe = match opt.source {
            OptionSource::Flag(flag) => flag.to_string(),
            OptionSource::Generic => format!("-O {}", opt.name),
        };
        let Some(option) = spec.options_for(operation).find(|o| o.name == opt.name) else {
            let supported: Vec<String> = spec
                .options_for(operation)
                .map(|o| o.flag.map(str::to_string).unwrap_or_else(|| format!("-O {}", o.name)))
                .collect();
            let list = if supported.is_empty() { "none".to_string() } else { supported.join(", ") };
            return Err(IrisError::new(
                ErrorCode::UnsupportedOption,
                format!("model '{}' does not support {describe} for {operation}", spec.id),
            )
            .with_hint(format!("options supported by this model for {operation}: {list}"))
            .with_detail("option", opt.name.clone()));
        };
        if resolved.contains(&opt.name) {
            return Err(IrisError::invalid(format!("option '{}' was given more than once", opt.name))
                .with_detail("option", opt.name.clone()));
        }
        let value = OptionValue::parse(&option.kind, &opt.value).map_err(|why| {
            IrisError::invalid(format!("invalid value '{}' for {describe}: {why}", opt.value))
                .with_detail("option", opt.name.clone())
        })?;
        resolved.insert(opt.name.clone(), value);
    }

    validate_inputs(spec, operation, inputs)?;

    if let Some(check) = spec.validate {
        check(&ValidationInput {
            operation,
            options: &resolved,
            input_images: inputs.images,
            has_mask: inputs.mask,
            has_first_frame: inputs.first_frame,
            has_last_frame: inputs.last_frame,
            reference_images: inputs.references,
        })?;
    }

    Ok(resolved)
}

fn validate_inputs(spec: &ModelSpec, op: Operation, inputs: InputCounts) -> Result<(), IrisError> {
    let unsupported = |what: &str| {
        IrisError::new(
            ErrorCode::UnsupportedOption,
            format!("model '{}' does not accept {what} for {op}", spec.id),
        )
    };
    match op {
        Operation::ImageGenerate => {
            if inputs.images > 0 || inputs.mask {
                return Err(IrisError::usage(
                    "image generate does not take input images; use `iris image edit --image ...`",
                ));
            }
        }
        Operation::ImageEdit => {
            if inputs.images == 0 {
                return Err(IrisError::usage("image edit requires at least one --image"));
            }
            let max = spec.inputs.max_input_images as usize;
            if inputs.images > max {
                return Err(IrisError::invalid(format!(
                    "model '{}' accepts at most {max} input image(s); got {}",
                    spec.id, inputs.images
                )));
            }
            if inputs.mask && !spec.inputs.mask {
                return Err(unsupported("--mask"));
            }
        }
        Operation::VideoGenerate => {
            if inputs.first_frame && !spec.inputs.first_frame {
                return Err(unsupported("--image (first frame)"));
            }
            if inputs.last_frame && !spec.inputs.last_frame {
                return Err(unsupported("--last-frame"));
            }
            let max = spec.inputs.max_reference_images as usize;
            if inputs.references > 0 && max == 0 {
                return Err(unsupported("--ref reference images"));
            }
            if inputs.references > max {
                return Err(IrisError::invalid(format!(
                    "model '{}' accepts at most {max} reference image(s); got {}",
                    spec.id, inputs.references
                )));
            }
        }
    }
    Ok(())
}
