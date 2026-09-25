//! The model catalog: every model Iris knows, with declared capabilities.
//!
//! Provider modules (`openai`, `gemini`, `veo`) contribute their static
//! declarations: the models, and the names Iris declines ([`DeclinedName`]).

pub mod gemini;
pub mod openai;
pub mod options;
pub mod types;
pub mod veo;

pub use options::{InputCounts, OptionSource, OptionValue, RawOption, ResolvedOptions, validate_request};
pub use types::*;

use crate::domain::{Operation, ProviderId};
use crate::error::{ErrorCode, IrisError};

/// Date the built-in catalog (capabilities and prices) was last checked against
/// provider documentation.
pub const CATALOG_AS_OF: &str = "2026-09-24";

/// Round a dollar amount to a millionth of a dollar so estimates print cleanly.
/// Every provider's estimates go through it, so they round the same way.
pub(crate) fn round_usd(amount: f64) -> f64 {
    (amount * 1e6).round() / 1e6
}

/// All built-in models, grouped by provider in a stable order.
pub fn all() -> impl Iterator<Item = &'static ModelSpec> {
    openai::MODELS.iter().chain(gemini::MODELS.iter()).chain(veo::MODELS.iter())
}

/// Look up a model by id or alias (case-sensitive ids; aliases are lowercase).
pub fn find(id_or_alias: &str) -> Option<&'static ModelSpec> {
    find_in(all(), id_or_alias)
}

/// [`find`] over an explicit model list. Ids and aliases are unique across the
/// built-in catalog (a test checks it), so at most one model matches there.
pub fn find_in(
    models: impl IntoIterator<Item = &'static ModelSpec>,
    id_or_alias: &str,
) -> Option<&'static ModelSpec> {
    models.into_iter().find(|m| m.id == id_or_alias || m.aliases.contains(&id_or_alias))
}

/// Every name Iris declines ([`DeclinedName`]), grouped by provider like [`all`].
pub fn declined_names() -> impl Iterator<Item = &'static DeclinedName> {
    openai::DECLINED.iter().chain(gemini::DECLINED.iter()).chain(veo::DECLINED.iter())
}

/// The declined name `name` is, if Iris deliberately gives it no model. Its
/// [`hint`](DeclinedName::hint) is the hint of the `unknown_model` error, and of the
/// `config_invalid` error for a configured model.
pub fn declined(name: &str) -> Option<&'static DeclinedName> {
    declined_names().find(|d| d.matches(name))
}

impl DeclinedName {
    /// Whether `name` is one of [`Self::names`] or a dated snapshot of one, or
    /// continues one of [`Self::families`], ignoring ASCII case.
    pub fn matches(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.names.iter().map(|n| n.to_ascii_lowercase()).any(|n| n == name || is_snapshot_of(&n, &name))
            || self.families.iter().any(|stem| {
                name.strip_prefix(&stem.to_ascii_lowercase())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
            })
    }

    /// Why Iris registers no model for the name, and what to use instead for a
    /// command running one of `ops` (any command when `ops` is empty, as for `models
    /// show`): each replacement that supports one of them, as declared, with its
    /// canonical id after a nickname (`nano-banana-2 (gemini-3.1-flash-image)`), or
    /// `otherwise` when none does.
    pub fn hint(&self, ops: &[Operation], otherwise: &str) -> String {
        let instead: Vec<String> = self
            .instead
            .iter()
            .filter_map(|name| find(name).map(|spec| (name, spec)))
            .filter(|(_, spec)| ops.is_empty() || ops.iter().any(|op| spec.supports(*op)))
            .map(|(name, spec)| {
                if spec.id != *name && !is_snapshot_of(spec.id, name) {
                    format!("{name} ({})", spec.id)
                } else {
                    name.to_string()
                }
            })
            .collect();
        if instead.is_empty() {
            return format!("{}; {otherwise}", self.reason);
        }
        format!("{}; use {}", self.reason, or_list(&instead))
    }
}

/// The `unknown_model` error for `name`, which no model of `models` (the catalog)
/// names: the model a command was given (`-m/--model`, or `models show <MODEL>`), or,
/// with `template`, the `--capabilities-from` model; `op` is the operation of the
/// generation command given the name, if any. The hint names the command that lists
/// the models for `op`, after, for a name Iris declines, why and what to use instead
/// for `op` ([`DeclinedName::hint`]), and otherwise the models `name` nearly names
/// ([`suggestions`], also in `details.suggestions`) as "did you mean …?", or, when
/// those are all models of other operations, which operations they are for (they are
/// not suggestions for `op`). Only an `-m` that is neither declined nor close to a
/// model gets the suggestion to use a model Iris does not know yet with
/// `--capabilities-from`. The app adds the models the command can use
/// (`details.candidates`).
pub fn unknown_model(
    models: &[&'static ModelSpec],
    name: &str,
    template: bool,
    op: Option<Operation>,
) -> IrisError {
    let message = if template {
        format!("--capabilities-from '{name}' is not a known model")
    } else {
        format!("unknown model '{name}'")
    };
    let listing = match op {
        Some(op) if template => {
            format!(
                "run `iris models list --operation {op}` and pass one of its models to --capabilities-from"
            )
        }
        Some(op) => format!("run `iris models list --operation {op}` and pass -m <MODEL>"),
        None => "run `iris models list` to see the models Iris knows".to_string(),
    };
    let declined = declined(name);
    let suggested = if declined.is_some() { Vec::new() } else { suggestions(models, name, op) };
    // Close models of other operations are named for what they do, not suggested.
    let elsewhere = match (&declined, suggested.as_slice(), op) {
        (None, [], Some(_)) => suggestions(models, name, None),
        _ => Vec::new(),
    };
    let hint = match (declined, suggested.as_slice()) {
        (Some(declined), _) => declined.hint(op.as_slice(), &listing),
        (None, [_, ..]) => format!("did you mean {}? otherwise {listing}", or_list(&suggested)),
        (None, []) if !elsewhere.is_empty() => format!("{}; {listing}", models_for(models, &elsewhere)),
        (None, []) if op.is_some() && !template => format!(
            "{listing}; to use a model Iris does not know yet, add --capabilities-from <KNOWN_MODEL> to declare \
             which known model's capabilities it has"
        ),
        (None, []) => listing,
    };
    IrisError::new(ErrorCode::UnknownModel, message).with_hint(hint).with_detail("suggestions", suggested)
}

/// The ids of the models of `models` (those implementing `op`, when there is one)
/// that `name` nearly names, in catalog order. Ignoring ASCII case, the first of
/// these rules that finds any model decides:
///
/// 1. `name` is an id or alias (`Nano-Banana-2`);
/// 2. `name` is a display name, or either name of a display name `A (B)`
///    (`GPT Image 2.5 Flare`, `Gemini 3 Pro Image`);
/// 3. `name` begins an id or alias (`gpt-image-2.5`, `veo-3.1-lite`);
/// 4. by [`words`]: every word of `name` that some model of `models` has (in its id,
///    an alias, or its display name) is a word of the model too, as it is or nearly
///    ([`word_matches`]); of those models, the ones with the most of these words
///    as they are (`veo3-fast`, `gpt-image-2.5-flair`, `nano-banana-lite`,
///    `sunburst`). A tier word such as `fast`, `lite`, `pro`, `flare`, or
///    `sunburst` is therefore never dropped, and `2.5` never becomes `2`. Words no
///    model has are ignored, but the words some model has must be more than half of
///    the name's words and include one of letters (`veo-4-ultra` and `sora-2`
///    suggest nothing: they may name models Iris does not know yet).
///
/// A later rule only runs when the earlier ones find nothing, so a closer match is
/// never listed next to looser ones.
pub fn suggestions(models: &[&'static ModelSpec], name: &str, op: Option<Operation>) -> Vec<&'static str> {
    let wanted = name.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return Vec::new();
    }
    let usable: Vec<&'static ModelSpec> =
        models.iter().copied().filter(|m| op.is_none_or(|op| m.supports(op))).collect();
    let ids = |m: &ModelSpec| -> Vec<String> {
        std::iter::once(m.id).chain(m.aliases.iter().copied()).map(str::to_ascii_lowercase).collect()
    };
    let display_names = |m: &ModelSpec| -> Vec<String> {
        let full = m.display_name.to_ascii_lowercase();
        let parts = full.strip_suffix(')').and_then(|rest| rest.split_once(" (")).map(|(a, b)| [a, b]);
        let mut all: Vec<String> = parts.into_iter().flatten().map(str::to_string).collect();
        all.push(full);
        all
    };
    let model_words = |m: &ModelSpec| -> Vec<String> {
        let mut all: Vec<String> = ids(m).iter().chain(&display_names(m)).flat_map(|n| words(n)).collect();
        all.sort();
        all.dedup();
        all
    };
    let mut typed = words(&wanted);
    typed.sort();
    typed.dedup();
    // Rule 4: the words of `name` some catalog model has, each of which a suggestion
    // must have too; a model's score is how many of them it has as they are.
    let has = |m: &ModelSpec, word: &str| model_words(m).iter().any(|w| word_matches(word, w));
    let known: Vec<&String> = typed.iter().filter(|t| models.iter().any(|m| has(m, t))).collect();
    let exact = |m: &ModelSpec| {
        let own = model_words(m);
        known.iter().filter(|t| own.contains(t)).count()
    };
    // They must be most of the name, and digits alone name no model: `veo-4-ultra`
    // and `sora-2` may be models Iris does not know yet.
    let named =
        known.len() * 2 > typed.len() && known.iter().any(|t| t.bytes().all(|b| b.is_ascii_alphabetic()));
    let has_all = |m: &ModelSpec| named && known.iter().all(|t| has(m, t));
    let best = usable.iter().filter(|m| has_all(m)).map(|m| exact(m)).max();
    let rules: [&dyn Fn(&ModelSpec) -> bool; 4] = [
        &|m| ids(m).contains(&wanted),
        &|m| display_names(m).contains(&wanted),
        &|m| ids(m).iter().any(|n| n.starts_with(&wanted)),
        &|m| has_all(m) && Some(exact(m)) == best,
    ];
    rules
        .iter()
        .map(|rule| usable.iter().filter(|m| rule(m)).map(|m| m.id).collect::<Vec<_>>())
        .find(|found| !found.is_empty())
        .unwrap_or_default()
}

/// The lowercase words of `name`: its runs of ASCII letters and of ASCII digits,
/// so it is split at every other character and between a letter and a digit
/// (`veo3-fast` → `veo`, `3`, `fast`; `gpt-image-2.5` → `gpt`, `image`, `2`, `5`).
fn words(name: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in name.chars().map(|c| c.to_ascii_lowercase()) {
        let continues =
            current.chars().last().is_some_and(|last| last.is_ascii_digit() == c.is_ascii_digit());
        if !c.is_ascii_alphanumeric() || !continues {
            words.extend((!current.is_empty()).then(|| std::mem::take(&mut current)));
        }
        if c.is_ascii_alphanumeric() {
            current.push(c);
        }
    }
    words.extend((!current.is_empty()).then_some(current));
    words
}

/// Whether the typed word `typed` stands for the catalog word `word`: it is `word`,
/// or both are words of at least four letters (no digits) whose Jaro-Winkler
/// similarity is at least 0.9. That takes a slip of a letter or two near the end
/// of a word (`flair` for `flare`, `sunbrust` for `sunburst`), but not another
/// word that merely starts the same way (`flash` is not `flare`).
fn word_matches(typed: &str, word: &str) -> bool {
    let long = |w: &str| w.len() >= 4 && w.bytes().all(|b| b.is_ascii_alphabetic());
    typed == word || (long(typed) && long(word) && strsim::jaro_winkler(typed, word) >= 0.9)
}

/// What the models `ids` of `models` are for, grouped by their operations:
/// `veo-3.1-lite-generate-preview is a video.generate model`, `a and b are
/// video.generate models`.
fn models_for(models: &[&'static ModelSpec], ids: &[&str]) -> String {
    let mut groups: Vec<(String, Vec<&str>)> = Vec::new();
    for id in ids {
        let Some(spec) = models.iter().find(|m| m.id == *id) else { continue };
        let ops: Vec<&str> = spec.operations.iter().map(|op| op.as_str()).collect();
        let ops = list_with(&ops, "and");
        match groups.iter_mut().find(|(o, _)| *o == ops) {
            Some((_, group)) => group.push(id),
            None => groups.push((ops, vec![id])),
        }
    }
    let clauses: Vec<String> = groups
        .iter()
        .map(|(ops, ids)| match ids.as_slice() {
            [one] => {
                let article = if ops.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
                format!("{one} is {article} {ops} model")
            }
            many => format!("{} are {ops} models", list_with(many, "and")),
        })
        .collect();
    clauses.join("; ")
}

/// `a`, `a or b`, `a, b, or c`.
fn or_list(items: &[impl AsRef<str>]) -> String {
    list_with(items, "or")
}

/// `a`, `a <conjunction> b`, `a, b, <conjunction> c`.
fn list_with(items: &[impl AsRef<str>], conjunction: &str) -> String {
    let items: Vec<&str> = items.iter().map(AsRef::as_ref).collect();
    match items.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [first, second] => format!("{first} {conjunction} {second}"),
        [rest @ .., last] => format!("{}, {conjunction} {last}", rest.join(", ")),
    }
}

/// The syntax of model ids `provider`'s adapter can send (checked for unknown ids
/// given with `--capabilities-from`; every catalog id satisfies it).
pub fn model_id_syntax(provider: ProviderId) -> ModelIdSyntax {
    match provider {
        ProviderId::OpenAi => openai::MODEL_ID_SYNTAX,
        ProviderId::Gemini => gemini::MODEL_ID_SYNTAX,
    }
}

/// Providers that implement an operation with at least one model, sorted.
pub fn providers_for(op: Operation) -> Vec<ProviderId> {
    let mut out: Vec<ProviderId> = all().filter(|m| m.supports(op)).map(|m| m.provider).collect();
    out.sort();
    out.dedup();
    out
}

/// How a model's capabilities were established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySource {
    /// Declared in the built-in catalog.
    Catalog,
    /// Unknown model id; capabilities borrowed from a known model via `--capabilities-from`.
    Borrowed { from: &'static str },
}

/// A resolved model: the id to send, and the declared capabilities that apply.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// Model id sent to the provider (a catalog id, or the user's unknown id).
    pub id: String,
    pub spec: &'static ModelSpec,
    pub source: CapabilitySource,
}

/// True if `candidate` is `base` followed by a `-YYYY-MM-DD` snapshot date.
fn is_snapshot_of(base: &str, candidate: &str) -> bool {
    let Some(suffix) = candidate.strip_prefix(base) else { return false };
    let b = suffix.as_bytes();
    b.len() == 11
        && b[0] == b'-'
        && b[5] == b'-'
        && b[8] == b'-'
        && [1, 2, 3, 4, 6, 7, 9, 10].iter().all(|&i| b[i].is_ascii_digit())
}

/// Resolve `--model` / `--capabilities-from` into a model.
///
/// * Known id or alias → catalog spec.
/// * Unknown id without `capabilities_from` → `unknown_model` ([`unknown_model`]).
/// * Unknown id with `capabilities_from` naming a known model → that model's spec,
///   sending the unknown id (caller emits warning `unverified_model_capabilities`).
pub fn resolve(model: &str, capabilities_from: Option<&str>) -> Result<ResolvedModel, IrisError> {
    let models: Vec<&'static ModelSpec> = all().collect();
    resolve_in(&models, model, capabilities_from, None)
}

/// [`resolve`] over an explicit model list (the app's injectable catalog uses this so
/// tests and production share one set of rules), for a generation command running
/// `op` when there is one (its `unknown_model` hint names the models for `op`).
pub fn resolve_in(
    models: &[&'static ModelSpec],
    model: &str,
    capabilities_from: Option<&str>,
    op: Option<Operation>,
) -> Result<ResolvedModel, IrisError> {
    let find = |id: &str| find_in(models.iter().copied(), id);
    if let Some(spec) = find(model) {
        if capabilities_from.is_some() {
            return Err(IrisError::usage(format!(
                "--capabilities-from is only for models Iris does not know; '{model}' is a known model"
            )));
        }
        // A dated snapshot alias (`<id>-YYYY-MM-DD`) pins that snapshot, so send it as given;
        // other aliases are Iris nicknames for the canonical id.
        let id = if is_snapshot_of(spec.id, model) { model } else { spec.id };
        return Ok(ResolvedModel { id: id.to_string(), spec, source: CapabilitySource::Catalog });
    }

    let Some(template) = capabilities_from else {
        return Err(unknown_model(models, model, false, op));
    };
    let Some(spec) = find(template) else {
        return Err(unknown_model(models, template, true, op));
    };
    let syntax = model_id_syntax(spec.provider);
    if !syntax.accepts(model) {
        return Err(IrisError::invalid(format!(
            "model id '{}' is not valid for provider {}: use {}",
            crate::redact::truncate(&crate::redact::scrub(model), 80),
            spec.provider,
            syntax.description
        ))
        .with_hint("check the --model value; nothing was sent"));
    }
    Ok(ResolvedModel { id: model.to_string(), spec, source: CapabilitySource::Borrowed { from: spec.id } })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Lookups return the first match, so a name declared twice would be shadowed
    /// silently: the whole catalog must be unambiguous. Every declared option default
    /// must also be a valid value.
    #[test]
    fn names_are_unique_and_option_defaults_are_valid() {
        let mut names: BTreeMap<String, &str> = BTreeMap::new();
        for m in all() {
            for name in std::iter::once(m.id).chain(m.aliases.iter().copied()) {
                // Case-insensitively, so two names never differ only in case.
                if let Some(other) = names.insert(name.to_ascii_lowercase(), m.id) {
                    panic!("'{name}' names both {other} and {}", m.id);
                }
            }
            // `models show` publishes each default typed by its option's kind.
            for o in m.options {
                if let Some(d) = o.default {
                    assert!(OptionValue::parse(&o.kind, d).is_ok(), "{}.{}: default {d:?}", m.id, o.name);
                }
            }
        }
    }

    #[test]
    fn every_catalog_id_and_alias_satisfies_its_providers_model_id_syntax() {
        for m in all() {
            let syntax = model_id_syntax(m.provider);
            for id in std::iter::once(m.id).chain(m.aliases.iter().copied()) {
                assert!(syntax.accepts(id), "{id}");
            }
        }
    }

    #[test]
    fn unknown_ids_follow_the_syntax_of_the_templates_provider() {
        let gemini = ["gemini-9.9-flash-image", "veo_4.0-x", "A1", &"a".repeat(128)];
        for id in gemini {
            assert!(resolve(id, Some("nano-banana-2")).is_ok(), "{id}");
            assert!(resolve(id, Some("veo")).is_ok(), "{id}");
        }
        for id in ["a:b", "bad/../id", "-lead", ".hidden", "a b", "a?b", "a%2Fb", "é", "", &"a".repeat(129)]
        {
            for template in ["nano-banana-2", "veo"] {
                let err = resolve(id, Some(template)).unwrap_err();
                assert_eq!(err.code, ErrorCode::InvalidArgument, "{id} {template}");
            }
        }
        for id in ["ft:gpt-image-2:org:custom:1", "org/model@v2", "-x"] {
            assert!(resolve(id, Some("gpt-image-2")).is_ok(), "{id}");
        }
        for id in ["a b", "a?b", "", &"a".repeat(201)] {
            assert_eq!(
                resolve(id, Some("gpt-image-2")).unwrap_err().code,
                ErrorCode::InvalidArgument,
                "{id}"
            );
        }
    }

    #[test]
    fn snapshot_aliases_are_sent_as_given_and_nicknames_are_canonicalized() {
        for m in all() {
            for alias in m.aliases {
                let resolved = resolve(alias, None).unwrap();
                assert_eq!(resolved.spec.id, m.id);
                if is_snapshot_of(m.id, alias) {
                    assert_eq!(resolved.id, *alias, "snapshot {alias} must be pinned");
                } else {
                    assert_eq!(resolved.id, m.id, "nickname {alias} must resolve to the canonical id");
                }
            }
        }
        assert!(is_snapshot_of("gpt-image-2", "gpt-image-2-2026-04-21"));
        assert!(!is_snapshot_of("gpt-image-2", "gpt-image-2-2026-04"));
        assert!(!is_snapshot_of("gpt-image-2", "gpt-image-2.5-sunburst"));
    }

    /// Every declined name, its dated snapshots, and every family stem find their own
    /// entry (no entry shadows another), none names a catalog model (catalog lookup
    /// comes first, so such an entry could never apply), and every replacement is a
    /// catalog model.
    #[test]
    fn declined_names_are_unambiguous_and_point_at_catalog_models() {
        for entry in declined_names() {
            assert!(!entry.instead.is_empty(), "{}", entry.reason);
            let snapshots = entry.names.iter().map(|n| format!("{n}-2026-01-31"));
            for name in entry.names.iter().chain(entry.families).map(|n| n.to_string()).chain(snapshots) {
                assert!(std::ptr::eq(declined(&name).unwrap(), entry), "{name} finds another entry");
                assert!(declined(&name.to_ascii_uppercase()).is_some(), "{name} ignoring case");
                assert!(find(&name).is_none(), "{name} is a catalog model");
            }
            for name in entry.instead {
                assert!(find(name).is_some(), "{name}, a replacement for {:?}", entry.names);
            }
            for ops in [&[][..], &[Operation::ImageGenerate], &[Operation::VideoGenerate]] {
                let hint = entry.hint(ops, "otherwise");
                assert!(hint.starts_with(entry.reason) && !hint.contains("--capabilities-from"), "{hint}");
            }
        }
        for m in all() {
            for name in std::iter::once(m.id).chain(m.aliases.iter().copied()) {
                assert!(declined(name).is_none(), "catalog model {name} is declined");
            }
        }
        // A family stem matches itself and its continuations with `-`, nothing else.
        let veo = declined("veo-3").unwrap();
        for name in ["veo-3", "veo-3-fast", "veo-3.0", "veo-3.0-generate-001", "VEO-3.0-FAST-GENERATE-001"] {
            assert!(veo.matches(name), "{name}");
        }
        for name in ["veo-30", "veo-3.1", "veo-3.1-generate-preview", "xveo-3"] {
            assert!(!veo.matches(name), "{name}");
        }
        // An exact name matches its dated snapshots, not other names that continue it.
        let gpt_image_1 = declined("gpt-image-1").unwrap();
        assert!(gpt_image_1.matches("gpt-image-1-2025-04-15"));
        assert!(!gpt_image_1.matches("gpt-image-1-mini") && !gpt_image_1.matches("gpt-image-1-2025-04"));
    }

    /// An unknown name's hint says how to go on: for a declined name why, and what
    /// to use instead that supports the command's operation (else where its models
    /// are listed), never `--capabilities-from`; for any other `-m`, the models of the
    /// command's operation and `--capabilities-from`; for a template or a name given
    /// without a command's operation, where the models are listed.
    #[test]
    fn unknown_model_hints_name_the_way_forward() {
        let models: Vec<&'static ModelSpec> = all().collect();
        let op = Some(Operation::ImageGenerate);
        let hint = |model: &str, template: Option<&str>, op| {
            let err = resolve_in(&models, model, template, op).unwrap_err();
            assert_eq!(err.code, ErrorCode::UnknownModel, "{model} {template:?}");
            err.hint.clone().unwrap()
        };
        let dalle = declined("dall-e-3").unwrap();
        let replacements =
            format!("{}; use gpt-image-2.5-sunburst, gpt-image-2.5-flare, or gpt-image-2", dalle.reason);
        assert_eq!(hint("dall-e-3", None, op), replacements);
        assert_eq!(hint("dall-e-3", None, None), replacements);
        assert_eq!(hint("my-model", Some("dall-e-3"), op), replacements);
        // None of them makes videos: the hint points at the video models instead.
        let video = Some(Operation::VideoGenerate);
        assert_eq!(
            hint("dall-e-3", None, video),
            format!(
                "{}; run `iris models list --operation video.generate` and pass -m <MODEL>",
                dalle.reason
            )
        );
        assert_eq!(
            hint("my-model", Some("dall-e-3"), video),
            format!(
                "{}; run `iris models list --operation video.generate` and pass one of its models to \
                 --capabilities-from",
                dalle.reason
            )
        );
        assert_eq!(
            hint("sora-2", None, op),
            "run `iris models list --operation image.generate` and pass -m <MODEL>; to use a model Iris does \
             not know yet, add --capabilities-from <KNOWN_MODEL> to declare which known model's capabilities it \
             has"
        );
        assert_eq!(
            hint("my-model", Some("sora-2"), op),
            "run `iris models list --operation image.generate` and pass one of its models to --capabilities-from"
        );
        assert_eq!(hint("sora-2", None, None), "run `iris models list` to see the models Iris knows");
    }

    #[test]
    fn names_split_into_lowercase_words_of_letters_or_digits() {
        assert_eq!(words("veo3-fast"), ["veo", "3", "fast"]);
        assert_eq!(words("GPT Image 2.5 Flare"), ["gpt", "image", "2", "5", "flare"]);
        assert_eq!(words("Nano Banana 2 (Gemini 3.1 Flash Image)")[..4], ["nano", "banana", "2", "gemini"]);
        assert_eq!(words("--4K__x"), ["4", "k", "x"]);
        assert!(words(" -. ").is_empty());
        // A slip near the end of a word of four or more letters, not another word.
        assert!(word_matches("flair", "flare") && word_matches("sunbrust", "sunburst"));
        assert!(!word_matches("flash", "flare") && !word_matches("fast", "flash"));
        assert!(!word_matches("pr0", "pro") && !word_matches("lit", "lite") && !word_matches("2", "25"));
    }

    /// A name that nearly names models of the operation suggests them by the first
    /// rule that finds any (case, display name, the start of an id or alias, the
    /// words), and its hint asks "did you mean …?" instead of offering
    /// `--capabilities-from`; a declined name suggests nothing. The word rule never
    /// drops a word some catalog model has: a tier (`fast`, `lite`, `flare`) or a
    /// version (`2.5`) that no model of the operation has suggests nothing.
    #[test]
    fn near_misses_suggest_the_models_they_nearly_name() {
        let models: Vec<&'static ModelSpec> = all().collect();
        let image = Some(Operation::ImageGenerate);
        let video = Some(Operation::VideoGenerate);
        let veo_3_1 =
            ["veo-3.1-fast-generate-preview", "veo-3.1-generate-preview", "veo-3.1-lite-generate-preview"];
        let gpt_image_2_5 = ["gpt-image-2.5-sunburst", "gpt-image-2.5-flare"];
        for (name, op, expected) in [
            ("Nano-Banana-2", image, &["gemini-3.1-flash-image"][..]),
            (" GPT-IMAGE-2 ", image, &["gpt-image-2"]),
            ("GPT Image 2.5 Flare", image, &["gpt-image-2.5-flare"]),
            ("gemini 3 pro image", image, &["gemini-3-pro-image"]),
            ("Nano Banana 2", image, &["gemini-3.1-flash-image"]),
            ("Veo 3.1", video, &["veo-3.1-generate-preview"]),
            ("gpt-image-2.5", image, &gpt_image_2_5),
            ("gpt-image-2.5", None, &gpt_image_2_5),
            ("veo-3.1-lite", video, &["veo-3.1-lite-generate-preview"]),
            ("veo-3.1", video, &veo_3_1),
            ("gpt-image-2.5-flare-latest", image, &["gpt-image-2.5-flare"]),
            ("veo3", video, &veo_3_1),
            ("veo3-fast", video, &["veo-3.1-fast-generate-preview"]),
            ("VEO3-FAST-HD", video, &["veo-3.1-fast-generate-preview"]),
            ("gpt-image-2.5-flair", image, &["gpt-image-2.5-flare"]),
            ("gpt-image-2.5-sunbrust", image, &["gpt-image-2.5-sunburst"]),
            ("gpt-image-2.5-mini", image, &gpt_image_2_5),
            ("flare", image, &["gpt-image-2.5-flare"]),
            ("sunburst", image, &["gpt-image-2.5-sunburst"]),
            ("nano-banana-lite", image, &["gemini-3.1-flash-lite-image"]),
            ("flash", image, &["gemini-3.1-flash-image", "gemini-3.1-flash-lite-image"]),
            // A word some model has is never dropped: none of these names a model.
            ("veo-lite-fast", video, &[]),
            ("nano-banana-2-pro", image, &[]),
            ("sora-2", image, &[]),
            ("model-3.1", video, &[]),
            ("veo-4-ultra", video, &[]),
            ("veo-4", video, &[]),
            // Only the models of the operation are suggested.
            ("veo-3.1", image, &[]),
            ("veo3-fast", image, &[]),
            ("Nano-Banana-2", video, &[]),
            ("", image, &[]),
        ] {
            assert_eq!(suggestions(&models, name, op), expected, "{name:?} for {op:?}");
        }

        let error =
            |model: &str, template: Option<&str>| resolve_in(&models, model, template, video).unwrap_err();
        let err = error("veo-3.1-lite", None);
        assert_eq!(
            err.hint.as_deref(),
            Some(
                "did you mean veo-3.1-lite-generate-preview? otherwise run `iris models list --operation \
                 video.generate` and pass -m <MODEL>"
            )
        );
        assert_eq!(err.details["suggestions"], serde_json::json!(["veo-3.1-lite-generate-preview"]));
        let err = error("my-model", Some("veo-3.1"));
        assert_eq!(
            err.hint.as_deref(),
            Some(
                "did you mean veo-3.1-fast-generate-preview, veo-3.1-generate-preview, or \
                 veo-3.1-lite-generate-preview? otherwise run `iris models list --operation video.generate` and \
                 pass one of its models to --capabilities-from"
            )
        );
        assert_eq!(error("veo-3", None).details["suggestions"], serde_json::json!([]));
        assert_eq!(error("sora-2", None).details["suggestions"], serde_json::json!([]));
    }
}
