//! Shared checks for the catalog tests (`tests/*_catalog.rs` include this file with
//! `#[path = "catalog_support.rs"] mod catalog_support;`); compiled on its own it is
//! an empty test crate.

#![allow(dead_code)]

use std::collections::BTreeSet;

use iris::catalog::{
    EstimateInput, InputCounts, ModelSpec, OptionKind, OptionSource, OptionValue, RawOption, ResolvedOptions,
    validate_request,
};
use iris::domain::Operation;

/// Input names a constraint may refer to (the plan's input roles).
pub const INPUT_NAMES: &[&str] = &["image", "mask", "first_frame", "last_frame", "reference"];

/// The built-in catalog, as `validate_request` takes it.
pub fn builtin() -> Vec<&'static ModelSpec> {
    iris::catalog::all().collect()
}

/// An option as `-O name=value` gives it.
fn raw(name: &str, value: &str) -> RawOption {
    RawOption { name: name.to_string(), value: value.to_string(), source: OptionSource::Generic }
}

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

/// Every combination of candidate values, one per option of `op` (see
/// [`candidates`]); `also` adds a value to try for an option it names.
fn option_combinations(spec: &ModelSpec, op: Operation, also: &[(&str, &str)]) -> Vec<Vec<RawOption>> {
    let mut combinations: Vec<Vec<RawOption>> = vec![Vec::new()];
    for option in spec.options_for(op) {
        let mut values = candidates(&option.kind, option.default);
        for (_, value) in also.iter().filter(|(name, _)| *name == option.name) {
            if !values.contains(&Some(value.to_string())) {
                values.push(Some(value.to_string()));
            }
        }
        combinations = combinations
            .into_iter()
            .flat_map(|combination| {
                values.iter().map(move |value| {
                    let mut combination = combination.clone();
                    combination.extend(value.as_deref().map(|value| raw(option.name, value)));
                    combination
                })
            })
            .collect();
    }
    combinations
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
        for raw in &option_combinations(spec, *op, &[]) {
            for counts in input_combinations(spec, *op) {
                let Err(e) = validate_request(spec, *op, raw, counts, &builtin()) else { continue };
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

/// Outputs a request with `options` asks for: the explicit or default `count`, else 1.
fn output_count(spec: &ModelSpec, op: Operation, options: &ResolvedOptions) -> i64 {
    if !spec.options_for(op).any(|o| o.name == "count") {
        return 1;
    }
    spec.effective(options, "count").and_then(|v| v.as_int()).unwrap_or(1)
}

/// The model's declared cheapest request (`Estimator::lowest`) is its cheapest valid
/// single-output request, as its own estimator prices it:
///
/// * the declared options are options of the model, validate for every operation of
///   the model (by its own rules), ask for one output, and are estimated the same for
///   every operation, as `ModelSpec::lowest_estimate` reports;
/// * no valid single-output combination of option values (every enum and boolean
///   value, integer bounds, unset; a pattern option unset, at its default, or at its
///   declared value) and input counts within the declared limits is estimated lower.
///
/// A pattern option has more values than these (OpenAI's `size`), so a provider's
/// tests also try that option's values themselves.
pub fn assert_lowest_estimate_is_the_cheapest(spec: &ModelSpec) {
    let Some(estimator) = spec.estimate else {
        assert!(spec.lowest_estimate().is_none(), "{}", spec.id);
        return;
    };
    let (options, lowest) =
        spec.lowest_estimate().unwrap_or_else(|| panic!("{}: its cheapest request has no estimate", spec.id));
    let declared: Vec<RawOption> = estimator.lowest.iter().map(|(name, value)| raw(name, value)).collect();
    let estimate = |op: Operation, options: &ResolvedOptions| {
        (estimator.estimate)(spec, &EstimateInput { operation: op, options, count: 1 })
    };
    let mut tried = 0;
    for &op in spec.operations {
        let minimal = input_combinations(spec, op)[0];
        let resolved = validate_request(spec, op, &declared, minimal, &builtin())
            .unwrap_or_else(|e| panic!("{} {op}: the cheapest request is invalid: {}", spec.id, e.message));
        assert_eq!(resolved, options, "{} {op}", spec.id);
        assert_eq!(output_count(spec, op, &resolved), 1, "{} {op}: one output", spec.id);
        assert_eq!(estimate(op, &resolved).as_ref(), Ok(&lowest), "{} {op}", spec.id);

        for raw in option_combinations(spec, op, estimator.lowest) {
            for counts in input_combinations(spec, op) {
                let Ok(options) = validate_request(spec, op, &raw, counts, &builtin()) else { continue };
                if output_count(spec, op, &options) != 1 {
                    continue;
                }
                tried += 1;
                if let Ok(e) = estimate(op, &options) {
                    assert!(
                        e.amount >= lowest.amount,
                        "{} {op}: {raw:?} is estimated at {} (below the declared cheapest request, {}): {}",
                        spec.id,
                        e.amount,
                        lowest.amount,
                        e.basis
                    );
                }
            }
        }
    }
    assert!(tried > 0, "{}", spec.id);
}
