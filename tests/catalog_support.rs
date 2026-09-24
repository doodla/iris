//! Shared checks for the catalog tests (`tests/*_catalog.rs` include this file with
//! `#[path = "catalog_support.rs"] mod catalog_support;`); compiled on its own it is
//! an empty test crate.

#![allow(dead_code)]

use std::collections::BTreeSet;

use iris::catalog::{
    InputCounts, ModelSpec, OptionKind, OptionSource, OptionValue, RawOption, validate_request,
};
use iris::domain::Operation;

/// Input names a constraint may refer to (the plan's input roles).
pub const INPUT_NAMES: &[&str] = &["image", "mask", "first_frame", "last_frame", "reference"];

/// Values to try for one option: unset, then a sample of valid values.
fn candidates(kind: &OptionKind, default: Option<&str>) -> Vec<Option<String>> {
    let mut values: Vec<Option<String>> = vec![None];
    match kind {
        OptionKind::Enum(values_) => values.extend(values_.iter().map(|v| Some(v.to_string()))),
        OptionKind::Integer { min, max } => {
            values.push(Some(min.to_string()));
            if max != min {
                values.push(Some(max.to_string()));
            }
        }
        OptionKind::Boolean => values.extend([Some("true".to_string()), Some("false".to_string())]),
        OptionKind::Text { .. } => values.push(Some("x".to_string())),
        OptionKind::Pattern { .. } => {
            if let Some(d) = default.filter(|d| OptionValue::parse(kind, d).is_ok()) {
                values.push(Some(d.to_string()));
            }
        }
    }
    values
}

/// Input combinations within the model's declared limits for `op`.
fn input_combinations(spec: &ModelSpec, op: Operation) -> Vec<InputCounts> {
    let i = &spec.inputs;
    match op {
        Operation::ImageGenerate => vec![InputCounts::default()],
        Operation::ImageEdit => {
            let mut out = Vec::new();
            let mut images = vec![1usize, i.max_input_images as usize];
            images.dedup();
            for images in images {
                for mask in [false, true] {
                    if mask && i.mask.is_none() {
                        continue;
                    }
                    out.push(InputCounts { images, mask, ..InputCounts::default() });
                }
            }
            out
        }
        Operation::VideoGenerate => {
            let mut out = Vec::new();
            let mut refs = vec![0usize, 1, i.max_reference_images as usize];
            refs.retain(|n| *n <= i.max_reference_images as usize);
            refs.dedup();
            for first_frame in [false, true] {
                for last_frame in [false, true] {
                    if (first_frame && !i.first_frame) || (last_frame && !i.last_frame) {
                        continue;
                    }
                    for references in &refs {
                        out.push(InputCounts {
                            first_frame,
                            last_frame,
                            references: *references,
                            ..InputCounts::default()
                        });
                    }
                }
            }
            out
        }
    }
}

/// Every cross-option rule `spec`'s validator enforces is declared in its
/// `constraints` (what `models show` publishes), and every declared constraint is
/// enforced:
///
/// * each combination of valid option values (every enum value, integer bounds,
///   unset) and input counts within the declared limits is validated; any
///   rejection must name a declared constraint in `details.constraint` (per-option
///   and input-count checks cannot fire, since only valid values are tried);
/// * each declared constraint rejects at least one combination, and names only
///   declared options and known input roles.
pub fn assert_constraints_cover_the_validator(spec: &ModelSpec) {
    let declared: Vec<&str> =
        spec.validate.map(|r| r.constraints.iter().map(|c| c.id).collect()).unwrap_or_default();
    if let Some(rules) = spec.validate {
        for c in rules.constraints {
            for option in c.options {
                assert!(
                    spec.option(option).is_some(),
                    "{}: constraint {} names unknown option {option}",
                    spec.id,
                    c.id
                );
            }
            for input in c.inputs {
                assert!(
                    INPUT_NAMES.contains(input),
                    "{}: constraint {} names unknown input {input}",
                    spec.id,
                    c.id
                );
            }
            assert!(!c.options.is_empty() || !c.inputs.is_empty(), "{}: {}", spec.id, c.id);
            assert!(!c.description.is_empty(), "{}: {}", spec.id, c.id);
        }
    }
    let unique: BTreeSet<&str> = declared.iter().copied().collect();
    assert_eq!(unique.len(), declared.len(), "{}: duplicate constraint ids", spec.id);

    let mut hit: BTreeSet<String> = BTreeSet::new();
    for op in spec.operations {
        // Every combination of candidate values, one per option of `op`.
        let mut combinations: Vec<Vec<RawOption>> = vec![Vec::new()];
        for option in spec.options_for(*op) {
            let values = candidates(&option.kind, option.default);
            combinations = combinations
                .into_iter()
                .flat_map(|raw| {
                    values.iter().map(move |value| {
                        let mut raw = raw.clone();
                        if let Some(value) = value {
                            raw.push(RawOption {
                                name: option.name.to_string(),
                                value: value.clone(),
                                source: OptionSource::Generic,
                            });
                        }
                        raw
                    })
                })
                .collect();
        }
        for raw in &combinations {
            for counts in input_combinations(spec, *op) {
                let Err(e) = validate_request(spec, *op, raw, counts) else { continue };
                let constraint = e.details.get("constraint").and_then(|c| c.as_str()).unwrap_or_else(|| {
                    panic!(
                        "{} {op}: {raw:?} {counts:?} was rejected by a rule that is not a declared constraint: \
                         {} ({})",
                        spec.id, e.message, e.code
                    )
                });
                assert!(declared.contains(&constraint), "{}: undeclared constraint {constraint}", spec.id);
                assert_eq!(e.code, iris::error::ErrorCode::InvalidArgument, "{}: {constraint}", spec.id);
                hit.insert(constraint.to_string());
            }
        }
    }
    let missing: Vec<&str> = declared.iter().copied().filter(|c| !hit.contains(*c)).collect();
    assert!(missing.is_empty(), "{}: declared constraints never enforced: {missing:?}", spec.id);
}
