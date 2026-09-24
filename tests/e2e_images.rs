//! End-to-end image scenarios: the built `iris` binary as a real process against
//! 127.0.0.1 mock servers that emulate the OpenAI Images API and the Gemini API.
//!
//! Covers scenarios 1 (OpenAI generate), 2 (OpenAI edit + mask), 3 (Gemini
//! generate/edit, thought parts, blocks), 4 (error mapping, retries, timeouts), and
//! the image half of 10 (secret hygiene with `-vv`). Every JSON envelope is
//! validated against the committed schema, including its command's result type.

mod support;

use std::path::Path;

use serde_json::{Value, json};
use support::*;

const PROMPT: &str = "a red fox in fresh snow, watercolor";

fn artifacts(v: &Value) -> &Vec<Value> {
    v["result"]["artifacts"].as_array().unwrap()
}

/// The saved artifact matches `bytes` on disk and in every reported field.
fn assert_artifact(art: &Value, path: &Path, bytes: &[u8], media_type: &str, (w, h): (u32, u32)) {
    assert_eq!(art["path"], path.to_str().unwrap(), "{art}");
    assert!(path.is_absolute());
    assert_eq!(std::fs::read(path).unwrap(), bytes, "the file holds exactly the provider's bytes");
    assert_eq!(art["media_type"], media_type);
    assert_eq!(art["bytes"], bytes.len() as u64);
    assert_eq!(art["sha256"], sha256_hex(bytes));
    assert_eq!(art["width"], w);
    assert_eq!(art["height"], h);
    assert!(art["duration_seconds"].is_null());
}

fn assert_ulid(value: &str) {
    assert_eq!(value.len(), 26, "{value}");
    assert!(value.chars().all(|c| c.is_ascii_alphanumeric()), "{value}");
}

// ----- scenario 1: OpenAI generate -------------------------------------------------------------

#[test]
fn openai_generate_saves_the_image_and_sends_the_documented_request() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    let image = png(64, 48);
    api.on("POST", OPENAI_GENERATIONS, openai_images(&[&image], "req_e2e_gen_1"));

    let out = sb
        .iris()
        .openai(&api)
        .args(["image", "generate", PROMPT, "--size", "1024x1024", "--quality", "low", "-o", "fox.png"])
        .arg("--json")
        .run();
    let v = out.ok();
    assert_eq!(v["command"], "image.generate");
    let r = &v["result"];
    assert_eq!(r["provider"], "openai");
    assert_eq!(r["model"], OPENAI_DEFAULT_MODEL);
    assert_eq!(r["operation"], "image.generate");
    assert_eq!(r["status"], "succeeded");
    assert_eq!(r["provider_request_id"], "req_e2e_gen_1");
    assert_eq!(artifacts(&v).len(), 1);
    assert_artifact(&artifacts(&v)[0], &sb.path("fox.png"), &image, "image/png", (64, 48));

    // The cost is an estimate from the reported usage, and says so.
    let cost = &r["cost_estimate"];
    assert_eq!(cost["estimated"], true, "{cost}");
    assert_eq!(cost["currency"], "USD");
    assert!((cost["amount"].as_f64().unwrap() - OPENAI_USAGE_ESTIMATE_USD).abs() < 1e-9, "{cost}");
    assert!(cost["basis"].as_str().unwrap().contains("estimate"), "{cost}");
    assert_eq!(r["usage"]["output_tokens"], 196);

    // Exactly one request, as documented: model always sent, only explicit options,
    // the key only in the Authorization header, a fresh client request id.
    let reqs = api.hits("POST", OPENAI_GENERATIONS);
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(header(req, "authorization").as_deref(), Some(format!("Bearer {OPENAI_KEY}").as_str()));
    assert_eq!(header(req, "content-type").as_deref(), Some("application/json"));
    assert_ulid(&header(req, "x-client-request-id").expect("X-Client-Request-Id"));
    assert!(header(req, "user-agent").unwrap().starts_with("iris/"), "{:?}", header(req, "user-agent"));
    assert!(req.url.query().is_none());
    assert_eq!(
        body_json(req),
        json!({
            "model": OPENAI_DEFAULT_MODEL,
            "prompt": PROMPT,
            "size": "1024x1024",
            "quality": "low",
            "output_format": "png"
        }),
        "-o fox.png selects the png format; nothing else is sent"
    );
    assert_eq!(api.total(), 1);

    // Progress went to stderr, never the prompt; no job record for a synchronous call.
    assert!(out.stderr.contains("openai"), "{}", out.stderr);
    assert!(!out.stderr.contains(PROMPT));
    assert!(!sb.jobs_dir().exists(), "synchronous image calls never create job records");
}

#[test]
fn openai_generate_in_human_mode_prints_saved_paths() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    let image = png(32, 32);
    api.on("POST", OPENAI_GENERATIONS, openai_images(&[&image], "req_e2e_gen_2"));

    let out = sb
        .iris()
        .openai(&api)
        .args(["image", "generate", PROMPT, "--size", "1024x1024", "--quality", "low", "-d", "pics"])
        .run();
    let stdout = out.human();
    let saved = stdout.strip_prefix("Saved ").and_then(|s| s.strip_suffix('\n')).expect(stdout);
    assert!(!saved.contains('\n'), "exactly one Saved line: {stdout}");
    let saved = Path::new(saved);
    assert_eq!(saved.parent().unwrap(), sb.path("pics"));
    let name = saved.file_name().unwrap().to_str().unwrap();
    let ulid = name.strip_prefix("iris-").and_then(|n| n.strip_suffix(".png")).expect(name);
    assert_ulid(ulid);
    assert_eq!(ulid, ulid.to_lowercase(), "Iris-generated names are lowercase");
    assert_eq!(std::fs::read(saved).unwrap(), image);
    assert!(out.stderr.contains("Estimated cost"), "{}", out.stderr);
    assert_eq!(api.total(), 1);
}

// ----- scenario 2: OpenAI edit -------------------------------------------------------------------

#[test]
fn openai_edit_sends_every_input_image_and_the_mask_as_data_urls() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    let (a, b, mask) = (png(64, 64), jpeg(32, 24), mask_png(64, 64));
    sb.write("a.png", &a);
    sb.write("b.jpg", &b);
    sb.write("mask.png", &mask);
    let result = png_colored(64, 64, [250, 250, 250]);
    api.on("POST", OPENAI_EDITS, openai_images(&[&result], "req_e2e_edit_1"));

    let v = sb
        .iris()
        .openai(&api)
        .args([
            "image",
            "edit",
            "add a tiny hat",
            "-i",
            "a.png",
            "-i",
            "b.jpg",
            "--mask",
            "mask.png",
            "--json",
        ])
        .run()
        .ok();
    assert_eq!(v["command"], "image.edit");
    assert_eq!(v["result"]["operation"], "image.edit");
    assert_eq!(v["result"]["provider_request_id"], "req_e2e_edit_1");
    let art = &artifacts(&v)[0];
    let path = Path::new(art["path"].as_str().unwrap()).to_path_buf();
    assert_eq!(path.parent().unwrap(), sb.work(), "default directory is the current directory");
    assert_artifact(art, &path, &result, "image/png", (64, 64));

    let reqs = api.hits("POST", OPENAI_EDITS);
    assert_eq!(reqs.len(), 1);
    assert_eq!(header(&reqs[0], "authorization").as_deref(), Some(format!("Bearer {OPENAI_KEY}").as_str()));
    assert_eq!(
        body_json(&reqs[0]),
        json!({
            "model": OPENAI_DEFAULT_MODEL,
            "prompt": "add a tiny hat",
            "images": [
                { "image_url": data_url("image/png", &a) },
                { "image_url": data_url("image/jpeg", &b) }
            ],
            "mask": { "image_url": data_url("image/png", &mask) }
        }),
        "inputs in order, types sniffed from the bytes, the mask as a PNG data URL"
    );
}

#[test]
fn openai_edit_with_a_mask_of_other_dimensions_is_rejected_before_any_request() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    sb.write("a.png", png(64, 64));
    sb.write("b.jpg", jpeg(32, 24));
    sb.write("mask.png", mask_png(32, 32));
    api.on("POST", OPENAI_EDITS, openai_images(&[&png(8, 8)], "req_never"));

    let out = sb
        .iris()
        .openai(&api)
        .args([
            "image",
            "edit",
            "add a tiny hat",
            "-i",
            "a.png",
            "-i",
            "b.jpg",
            "--mask",
            "mask.png",
            "--json",
        ])
        .run();
    let v = out.err(2, "input_file_invalid");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(message.contains("32x32") && message.contains("64x64"), "{message}");
    assert_eq!(api.total(), 0, "nothing was sent");
    assert_eq!(files_in(&sb.work()), ["a.png", "b.jpg", "mask.png"], "nothing was saved");
}

// ----- scenario 3: Gemini generate / edit ------------------------------------------------------

#[test]
fn gemini_generate_keeps_the_final_image_and_ignores_thought_parts() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    let (draft, fin) = (png_colored(16, 16, [1, 2, 3]), png(48, 27));
    let route = gemini_generate_path(GEMINI_DEFAULT_IMAGE_MODEL);
    let mut draft_part = inline_part("image/png", &draft);
    draft_part["thought"] = json!(true);
    api.on(
        "POST",
        &route,
        gemini_parts(json!([
            { "text": "Planning the composition: a lighthouse, dusk light.", "thought": true },
            draft_part,
            inline_part("image/png", &fin)
        ])),
    );

    let prompt = "a lighthouse at dusk";
    let v = sb
        .iris()
        .gemini(&api)
        .args(["image", "generate", prompt, "--provider", "gemini", "--aspect-ratio", "16:9"])
        .args(["--resolution", "1K", "--json"])
        .run()
        .ok();
    let r = &v["result"];
    assert_eq!(r["provider"], "gemini");
    assert_eq!(r["model"], GEMINI_DEFAULT_IMAGE_MODEL);
    assert_eq!(artifacts(&v).len(), 1, "the thought image is not an output: {v}");
    let art = &artifacts(&v)[0];
    assert_artifact(art, Path::new(art["path"].as_str().unwrap()), &fin, "image/png", (48, 27));
    assert!(r["text"].is_null(), "thought text is not model output: {v}");
    assert!(!warning_codes(&v).contains(&"provider_text_output".to_string()), "{v}");
    // Estimated from the reported usageMetadata (no image-token itemization, so all
    // 1120 candidate tokens at the $60/1M image rate): 12 × $0.50 + 1120 × $60 + 40 × $3.
    let cost = &r["cost_estimate"];
    assert_eq!(cost["estimated"], true, "{v}");
    assert!((cost["amount"].as_f64().unwrap() - 0.067326).abs() < 1e-9, "{cost}");

    let reqs = api.hits("POST", &route);
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(header(req, "x-goog-api-key").as_deref(), Some(GEMINI_KEY));
    assert!(header(req, "authorization").is_none());
    assert!(req.url.query().is_none(), "the key is never a ?key= parameter: {}", req.url);
    assert_eq!(
        body_json(req),
        json!({
            "contents": [ { "role": "user", "parts": [ { "text": prompt } ] } ],
            "generationConfig": {
                "responseModalities": ["IMAGE"],
                "imageConfig": { "aspectRatio": "16:9", "imageSize": "1K" }
            },
            "store": false
        })
    );
}

#[test]
fn gemini_edit_sends_reference_images_inline_in_order() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    let (a, b) = (png(40, 40), jpeg(20, 30));
    sb.write("a.png", &a);
    sb.write("b.jpg", &b);
    let out_img = jpeg(64, 64);
    let route = gemini_generate_path(GEMINI_DEFAULT_IMAGE_MODEL);
    api.on("POST", &route, gemini_parts(json!([inline_part("image/jpeg", &out_img)])));

    let v = sb
        .iris()
        .gemini(&api)
        .args(["image", "edit", "put the two together", "--provider", "gemini", "-i", "a.png", "-i", "b.jpg"])
        .args(["-o", "combined", "--json"])
        .run()
        .ok();
    assert_eq!(v["result"]["operation"], "image.edit");
    let art = &artifacts(&v)[0];
    let path = Path::new(art["path"].as_str().unwrap()).to_path_buf();
    assert_eq!(path.parent().unwrap(), sb.work());
    assert!(
        path.extension().is_some_and(|e| e == "jpg" || e == "jpeg"),
        "an extension-less -o gets the JPEG extension: {}",
        path.display()
    );
    assert!(warning_codes(&v).contains(&"output_extension_adjusted".to_string()), "{v}");
    assert_artifact(art, &path, &out_img, "image/jpeg", (64, 64));

    let reqs = api.hits("POST", &route);
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        body_json(&reqs[0])["contents"],
        json!([{
            "role": "user",
            "parts": [
                { "text": "put the two together" },
                { "inlineData": { "mimeType": "image/png", "data": b64(&a) } },
                { "inlineData": { "mimeType": "image/jpeg", "data": b64(&b) } }
            ]
        }])
    );
}

#[test]
fn gemini_blocks_are_content_blocked_and_save_nothing() {
    let route = gemini_generate_path(GEMINI_DEFAULT_IMAGE_MODEL);
    let blocked_prompt = json_response(200, json!({ "promptFeedback": { "blockReason": "SAFETY" } }));
    let blocked_output = json_response(
        200,
        json!({ "candidates": [ { "content": { "role": "model", "parts": [] }, "finishReason": "IMAGE_SAFETY" } ] }),
    );
    for (answer, provider_code) in [(blocked_prompt, "SAFETY"), (blocked_output, "IMAGE_SAFETY")] {
        let sb = Sandbox::new();
        let api = MockApi::start();
        api.on("POST", &route, answer);
        let v = sb
            .iris()
            .gemini(&api)
            .args(["image", "generate", "something", "--provider", "gemini", "--json"])
            .run()
            .err(1, "content_blocked");
        assert_eq!(v["error"]["category"], "content");
        assert_eq!(v["error"]["retryable"], false);
        assert_eq!(v["error"]["provider_code"], provider_code);
        assert_eq!(api.total(), 1, "blocks are never retried");
        assert!(files_in(&sb.work()).is_empty(), "nothing was saved");
    }
}

// ----- paid output is never discarded -------------------------------------------------------------

#[test]
fn an_unusable_openai_item_never_costs_the_good_image() {
    let image = png(20, 20);
    let body = json!({
        "created": 1_790_000_000,
        "data": [ { "b64_json": b64(&image) }, { "b64_json": b64(br#"{"error": {"message": "x"}}"#) } ],
        "output_format": "png",
        "usage": { "input_tokens": 50, "output_tokens": 196, "total_tokens": 246 }
    });
    let (sb, api, out) = openai_run(
        json_response(200, body).insert_header("x-request-id", "req_e2e_mixed"),
        &["-n", "2", "-o", "g.png"],
    );
    let v = out.ok();
    assert_eq!(api.total(), 1);
    assert_eq!(artifacts(&v).len(), 1, "{v}");
    let art = &artifacts(&v)[0];
    assert_artifact(art, Path::new(art["path"].as_str().unwrap()), &image, "image/png", (20, 20));
    let unusable: Vec<&Value> =
        v["warnings"].as_array().unwrap().iter().filter(|w| w["code"] == "output_item_unusable").collect();
    assert_eq!(unusable.len(), 1, "{v}");
    assert!(unusable[0]["message"].as_str().unwrap().contains("image 1 "), "{v}");
    assert!(v["result"]["cost_estimate"]["amount"].as_f64().is_some(), "usage still gives an estimate: {v}");
    assert_eq!(files_in(&sb.work()).len(), 1, "{:?}", files_in(&sb.work()));
}

#[test]
fn a_mislabeled_or_unlabeled_gemini_image_is_saved_under_its_real_type() {
    let image = jpeg(24, 16);
    let parts = [
        ("mislabeled", inline_part("image/png", &image)),
        ("unlabeled", json!({ "inlineData": { "data": b64(&image) } })),
    ];
    for (name, part) in parts {
        let (sb, api, out) = gemini_run(gemini_parts(json!([part])), &["-o", "gm.png"]);
        let v = out.ok();
        assert_eq!(api.total(), 1, "{name}");
        let art = &artifacts(&v)[0];
        assert_artifact(art, &sb.path("gm.jpg"), &image, "image/jpeg", (24, 16));
        let codes = warning_codes(&v);
        assert!(codes.contains(&"output_format_mismatch".to_string()), "{name}: {v}");
        assert!(codes.contains(&"output_extension_adjusted".to_string()), "{name}: {v}");
        assert!(!sb.path("gm.png").exists(), "{name}");
    }
}

#[test]
fn an_image_that_cannot_be_saved_where_requested_is_kept_in_the_state_directory() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    let image = png(10, 10);
    api.on(
        "POST",
        OPENAI_GENERATIONS,
        openai_images(&[&image], "req_e2e_fallback").set_delay(std::time::Duration::from_secs(3)),
    );
    let child = sb
        .iris()
        .openai(&api)
        .args(["image", "generate", PROMPT, "--size", "1024x1024", "--quality", "low"])
        .args(["-o", "out/pic.png", "--json"])
        .spawn();
    // The request is in flight, so preflight created out/: put a file in its place.
    api.wait_for("POST", OPENAI_GENERATIONS, 1, std::time::Duration::from_secs(30));
    std::fs::remove_dir_all(sb.path("out")).unwrap();
    std::fs::write(sb.path("out"), b"in the way").unwrap();
    let out = child.finish();

    let v = out.ok();
    assert_eq!(api.total(), 1, "the image is saved elsewhere, never generated again");
    let art = &artifacts(&v)[0];
    let path = Path::new(art["path"].as_str().unwrap());
    assert_eq!(path.parent().unwrap(), sb.state().join("unsaved"), "{v}");
    assert_artifact(art, path, &image, "image/png", (10, 10));
    let warning = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "output_saved_elsewhere")
        .unwrap_or_else(|| panic!("no output_saved_elsewhere warning: {v}"));
    assert!(warning["message"].as_str().unwrap().contains(path.to_str().unwrap()), "{v}");
    assert_eq!(std::fs::read(sb.path("out")).unwrap(), b"in the way", "the file in the way is untouched");
}

#[test]
fn a_missing_key_creates_no_output_directory() {
    let sb = Sandbox::new();
    for extra in [["-d", "newdir"], ["-o", "deep/er/x.png"]] {
        let out = sb
            .iris()
            .args(["image", "generate", PROMPT, "--size", "1024x1024", "--quality", "low", "--json"])
            .args(extra)
            .run();
        let v = out.err(3, "missing_credentials");
        assert!(v["error"]["message"].as_str().unwrap().contains("OPENAI_API_KEY"), "{v}");
    }
    assert!(files_in(&sb.work()).is_empty(), "{:?}", files_in(&sb.work()));

    // A dry run needs no key and creates nothing either.
    let out = sb.iris().args(["image", "generate", PROMPT, "-d", "planned", "--dry-run", "--json"]).run();
    out.ok();
    assert!(files_in(&sb.work()).is_empty(), "{:?}", files_in(&sb.work()));
}

// ----- scenario 4: error mapping through the process --------------------------------------------

/// Run one OpenAI generation against `answer` and return the output and the mock.
fn openai_run(answer: impl wiremock::Respond + 'static, extra: &[&str]) -> (Sandbox, MockApi, Out) {
    let sb = Sandbox::new();
    let api = MockApi::start();
    api.on("POST", OPENAI_GENERATIONS, answer);
    let out = sb
        .iris()
        .openai(&api)
        .args(["image", "generate", PROMPT, "--size", "1024x1024", "--quality", "low"])
        .args(extra)
        .arg("--json")
        .run();
    (sb, api, out)
}

/// Run one Gemini generation against `answer`.
fn gemini_run(answer: impl wiremock::Respond + 'static, extra: &[&str]) -> (Sandbox, MockApi, Out) {
    let sb = Sandbox::new();
    let api = MockApi::start();
    api.on("POST", &gemini_generate_path(GEMINI_DEFAULT_IMAGE_MODEL), answer);
    let out = sb
        .iris()
        .gemini(&api)
        .args(["image", "generate", PROMPT, "--provider", "gemini"])
        .args(extra)
        .arg("--json")
        .run();
    (sb, api, out)
}

#[test]
fn authentication_failures_exit_3_without_retrying() {
    // OpenAI 401; the provider echoes the key in its message, which Iris scrubs.
    let (sb, api, out) = openai_run(
        openai_error(
            401,
            "invalid_request_error",
            Some("invalid_api_key"),
            &format!("Incorrect API key provided: {OPENAI_KEY}."),
        ),
        &[],
    );
    let v = out.err(3, "authentication_failed");
    assert_eq!(v["error"]["category"], "auth");
    assert_eq!(v["error"]["provider"], "openai");
    assert_eq!(v["error"]["provider_status"], 401);
    assert_eq!(v["error"]["provider_request_id"], "req_e2e_error");
    assert_eq!(api.total(), 1);
    assert!(files_in(&sb.work()).is_empty());

    // Gemini: HTTP 400 with ErrorInfo.reason API_KEY_INVALID (how Google reports a bad key).
    let (_sb, api, out) = gemini_run(
        google_error(
            400,
            "INVALID_ARGUMENT",
            "API key not valid. Please pass a valid API key.",
            json!([{ "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "API_KEY_INVALID", "domain": "googleapis.com" }]),
        ),
        &[],
    );
    let v = out.err(3, "authentication_failed");
    assert_eq!(v["error"]["provider"], "gemini");
    assert_eq!(v["error"]["provider_status"], 400);
    assert_eq!(api.total(), 1);
}

#[test]
fn quota_and_billing_refusals_exit_3_without_retrying() {
    let quota_429 = openai_error(
        429,
        "insufficient_quota",
        Some("insufficient_quota"),
        "You exceeded your current quota, please check your plan and billing details.",
    )
    .insert_header("retry-after-ms", "10");
    let payment_402 = openai_error(402, "billing_error", None, "Payment required.");
    for answer in [quota_429, payment_402] {
        let (_sb, api, out) = openai_run(answer, &[]);
        let v = out.err(3, "quota_exceeded");
        assert_eq!(v["error"]["category"], "quota");
        assert_eq!(v["error"]["retryable"], false);
        assert_eq!(api.total(), 1, "quota exhaustion is never retried");
    }

    let (_sb, api, out) = gemini_run(
        google_error(402, "FAILED_PRECONDITION", "Your prepayment credits are depleted.", json!([])),
        &[],
    );
    let v = out.err(3, "quota_exceeded");
    assert_eq!(v["error"]["provider"], "gemini");
    assert_eq!(api.total(), 1);
}

#[test]
fn a_rate_limited_paid_request_is_retried_and_then_succeeds() {
    let image = png(24, 24);
    let limited =
        openai_error(429, "requests", Some("rate_limit_exceeded"), "Rate limit reached for requests.")
            .insert_header("retry-after-ms", "50");
    let (sb, api, out) = openai_run(
        Switch::sequence(vec![limited, openai_images(&[&image], "req_e2e_retry")]),
        &["-o", "r.png"],
    );
    let v = out.ok();
    assert_eq!(api.count("POST", OPENAI_GENERATIONS), 2, "one rate-limited attempt, one success");
    assert_eq!(v["result"]["provider_request_id"], "req_e2e_retry");
    assert_eq!(std::fs::read(sb.path("r.png")).unwrap(), image);
    assert_eq!(files_in(&sb.work()), ["r.png"], "one image saved once");

    // Gemini's 429 RESOURCE_EXHAUSTED with a RetryInfo delay.
    let limited = google_error(
        429,
        "RESOURCE_EXHAUSTED",
        "Resource has been exhausted (e.g. check quota).",
        json!([{ "@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "0.05s" }]),
    );
    let (_sb, api, out) = gemini_run(
        Switch::sequence(vec![limited, gemini_parts(json!([inline_part("image/png", &image)]))]),
        &[],
    );
    out.ok();
    assert_eq!(api.total(), 2);

    // A requested delay beyond the automatic-wait cap (60 s) is not waited out: one
    // attempt, then rate_limited with the delay, for the caller to decide.
    let limited =
        openai_error(429, "requests", Some("rate_limit_exceeded"), "Rate limit reached for requests.")
            .insert_header("retry-after", "120");
    let (_sb, api, out) = openai_run(limited, &[]);
    let v = out.err(1, "rate_limited");
    assert_eq!(v["error"]["retryable"], true);
    assert_eq!(v["error"]["retry_after_seconds"], 120);
    assert_eq!(api.total(), 1);
    assert!(out.elapsed < std::time::Duration::from_secs(30), "{:?}", out.elapsed);
}

/// A paid request that may have been billed is never presented as retryable.
fn assert_not_retryable_if_charged(v: &Value) {
    if v["error"]["details"]["charge_possible"] == true {
        assert_ne!(v["error"]["retryable"], true, "charge_possible with retryable true: {v}");
    }
}

/// The error of a paid synchronous call whose outcome Iris cannot know: exit 5,
/// not retryable, possibly charged, and no job to resume.
fn assert_uncertain(out: &Out) -> Value {
    let v = out.err(5, "submission_uncertain");
    let e = &v["error"];
    assert_eq!(e["category"], "uncertain", "{v}");
    assert_eq!(e["retryable"], false, "{v}");
    assert_eq!(e["details"]["charge_possible"], true, "{v}");
    assert!(e["job_id"].is_null() && e["job_status"].is_null(), "a synchronous call has no job: {v}");
    assert!(e["hint"].as_str().unwrap().contains("did not retry"), "{v}");
    assert_not_retryable_if_charged(&v);
    v
}

#[test]
fn a_server_error_on_a_paid_image_request_is_reported_once_and_never_retried() {
    // OpenAI may have processed a request it answered with a 5xx: uncertain (exit 5).
    let (sb, api, out) = openai_run(openai_error(500, "server_error", None, "The server had an error."), &[]);
    let v = assert_uncertain(&out);
    assert_eq!(v["error"]["provider"], "openai");
    assert_eq!(v["error"]["provider_status"], 500);
    assert_eq!(v["error"]["provider_request_id"], "req_e2e_error");
    let sent = header(&api.hits("POST", OPENAI_GENERATIONS)[0], "x-client-request-id").unwrap();
    assert_eq!(v["error"]["details"]["client_request_id"], sent.as_str(), "{v}");
    assert_eq!(api.total(), 1, "a paid request that may have been processed is never resent");
    assert!(files_in(&sb.work()).is_empty());

    // Google does not charge a request that failed with a 5xx: an ordinary, retryable error.
    let (_sb, api, out) =
        gemini_run(google_error(500, "INTERNAL", "An internal error has occurred.", json!([])), &[]);
    let v = out.err(1, "provider_error");
    assert_eq!(v["error"]["retryable"], true, "{v}");
    assert!(v["error"]["details"].get("charge_possible").is_none(), "{v}");
    assert_eq!(api.total(), 1);
}

#[test]
fn an_overloaded_openai_503_is_retried_and_then_succeeds() {
    let image = png(12, 12);
    let overloaded = openai_error(
        503,
        "service_unavailable_error",
        Some("server_is_overloaded"),
        "The model is overloaded.",
    )
    .insert_header("retry-after-ms", "20");
    let (sb, api, out) = openai_run(
        Switch::sequence(vec![overloaded, openai_images(&[&image], "req_e2e_overload")]),
        &["-o", "o.png"],
    );
    out.ok();
    assert_eq!(api.count("POST", OPENAI_GENERATIONS), 2, "OpenAI documents the 503 as not processed");
    assert_eq!(std::fs::read(sb.path("o.png")).unwrap(), image);
}

#[test]
fn a_response_slower_than_the_configured_timeout_is_submission_uncertain() {
    let slow = openai_images(&[&png(8, 8)], "req_slow").set_delay(std::time::Duration::from_secs(4));
    let sb = Sandbox::new();
    let api = MockApi::start();
    api.on("POST", OPENAI_GENERATIONS, slow);
    let config = sb.config("iris.toml", "[providers.openai]\nrequest_timeout = \"1s\"\n");
    let out = sb
        .iris()
        .openai(&api)
        .args(["image", "generate", PROMPT, "--size", "1024x1024", "--quality", "low", "--json"])
        .arg("--config")
        .arg(&config)
        .run();
    let v = assert_uncertain(&out);
    assert_eq!(v["error"]["details"]["transport"], "timeout", "{v}");
    assert!(v["error"]["details"]["client_request_id"].is_string(), "{v}");
    assert_eq!(api.total(), 1, "never resent");
    assert!(out.elapsed < std::time::Duration::from_secs(4), "the 1s timeout applied: {:?}", out.elapsed);
    assert!(files_in(&sb.work()).is_empty());

    // The same through the Gemini adapter.
    let sb = Sandbox::new();
    let api = MockApi::start();
    let route = gemini_generate_path(GEMINI_DEFAULT_IMAGE_MODEL);
    api.on(
        "POST",
        &route,
        gemini_parts(json!([inline_part("image/png", &png(8, 8))]))
            .set_delay(std::time::Duration::from_secs(4)),
    );
    let config = sb.config("iris.toml", "[providers.gemini]\nrequest_timeout = \"1s\"\n");
    let out = sb
        .iris()
        .gemini(&api)
        .args(["image", "generate", PROMPT, "--provider", "gemini", "--json", "--config"])
        .arg(&config)
        .run();
    let v = assert_uncertain(&out);
    assert_eq!(v["error"]["provider"], "gemini");
    assert_eq!(v["error"]["details"]["transport"], "timeout", "{v}");
    assert_eq!(api.total(), 1);
}

/// A raw 127.0.0.1 server that reads each request completely (head and
/// `Content-Length` body) and closes the connection without answering: a
/// connection lost after the request was sent. Returns its origin and the number of
/// requests read.
fn dropping_server() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::Read as _;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 16 * 1024];
            let mut head_end = None;
            while head_end.is_none() {
                match stream.read(&mut chunk) {
                    Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                    _ => break,
                }
                head_end = buf.windows(4).position(|w| w == b"\r\n\r\n");
            }
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
            let body_len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < head_end + 4 + body_len {
                match stream.read(&mut chunk) {
                    Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                    _ => break,
                }
            }
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Dropping the stream closes the connection with no response.
        }
    });
    (origin, requests)
}

#[test]
fn a_connection_dropped_after_sending_is_submission_uncertain_not_a_timeout() {
    // OpenAI.
    let sb = Sandbox::new();
    let (origin, requests) = dropping_server();
    let out = sb
        .iris()
        .env("IRIS_OPENAI_BASE_URL", format!("{origin}/v1"))
        .env("OPENAI_API_KEY", OPENAI_KEY)
        .args(["image", "generate", PROMPT, "--size", "1024x1024", "--quality", "low", "--json"])
        .run();
    let v = assert_uncertain(&out);
    assert_eq!(v["error"]["provider"], "openai");
    assert_eq!(v["error"]["details"]["transport"], "other", "nothing timed out: {v}");
    assert!(v["error"]["details"]["client_request_id"].is_string(), "{v}");
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1, "never resent");
    assert!(files_in(&sb.work()).is_empty());

    // Gemini.
    let sb = Sandbox::new();
    let (origin, requests) = dropping_server();
    let out = sb
        .iris()
        .env("IRIS_GEMINI_BASE_URL", &origin)
        .env("GEMINI_API_KEY", GEMINI_KEY)
        .args(["image", "generate", PROMPT, "--provider", "gemini", "--json"])
        .run();
    let v = assert_uncertain(&out);
    assert_eq!(v["error"]["provider"], "gemini");
    assert_eq!(v["error"]["details"]["transport"], "other", "nothing timed out: {v}");
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1, "never resent");
}

#[test]
fn ctrl_c_during_a_paid_image_call_exits_130_and_is_not_retryable() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    api.on(
        "POST",
        OPENAI_GENERATIONS,
        openai_images(&[&png(8, 8)], "req_e2e_int").set_delay(std::time::Duration::from_secs(10)),
    );
    let child = sb
        .iris()
        .openai(&api)
        .args(["image", "generate", PROMPT, "--size", "1024x1024", "--quality", "low", "--json"])
        .spawn();
    // The request was received, so the Ctrl-C handler is installed and the call is in flight.
    api.wait_for("POST", OPENAI_GENERATIONS, 1, std::time::Duration::from_secs(30));
    child.interrupt();
    let out = child.finish();
    let v = out.err(130, "interrupted");
    assert_eq!(v["error"]["retryable"], false, "{v}");
    assert_eq!(v["error"]["details"]["charge_possible"], true, "{v}");
    assert!(v["error"]["hint"].as_str().unwrap().contains("did not retry"), "{v}");
    assert_not_retryable_if_charged(&v);
    assert!(out.elapsed < std::time::Duration::from_secs(10), "{:?}", out.elapsed);
    assert_eq!(api.total(), 1);
    assert!(files_in(&sb.work()).is_empty());
}

// ----- scenario 10 (images): secret hygiene with -vv ----------------------------------------------

#[test]
fn verbose_image_runs_never_reveal_the_key_or_signed_urls() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    let image = png(16, 16);
    api.on("POST", OPENAI_GENERATIONS, openai_images(&[&image], "req_e2e_vv"));
    let out = sb
        .iris()
        .openai(&api)
        .env("GEMINI_API_KEY", GEMINI_KEY)
        .args(["-vv", "image", "generate", PROMPT, "--size", "1024x1024", "--quality", "low", "--json"])
        .run();
    out.ok();
    assert!(out.stderr.contains("http response"), "-vv logs request metadata: {}", out.stderr);
    assert!(!urls_in(&out.stderr).is_empty(), "-vv logs the (redacted) URL: {}", out.stderr);
    assert!(!out.stderr.contains(PROMPT), "prompts are never logged");
    assert_printed_urls_redacted(&out.stderr);

    // A provider error that echoes the key and a signed URL: both are scrubbed.
    let api = MockApi::start();
    api.on(
        "POST",
        OPENAI_GENERATIONS,
        openai_error(
            400,
            "invalid_request_error",
            None,
            &format!(
                "Bad request for key {OPENAI_KEY}; see https://files.example.test/x?sig={SIGNATURE}&alt=media"
            ),
        ),
    );
    let out = sb
        .iris()
        .openai(&api)
        .args(["-vv", "image", "generate", PROMPT, "--size", "1024x1024", "--quality", "low", "--json"])
        .run();
    // Exit 2 here is a request the provider refused outright (sent, rejected, not
    // charged), not a local validation failure: `provider_status` tells them apart.
    let v = out.err(2, "invalid_argument");
    assert_eq!(v["error"]["provider_status"], 400, "{v}");
    assert_eq!(api.count("POST", OPENAI_GENERATIONS), 1);
    for text in [&out.stdout, &out.stderr] {
        assert!(!text.contains(SIGNATURE), "signed URL value leaked: {text}");
    }
    assert_printed_urls_redacted(&out.stdout);
    let message = v["error"]["message"].as_str().unwrap();
    assert!(message.contains("key [REDACTED]"), "the echoed key is scrubbed: {message}");
    assert!(message.contains("sig=REDACTED"), "the signed URL is redacted: {message}");
    assert!(!urls_in(&out.stdout).is_empty());

    for dir in [sb.work(), sb.state(), sb.home()] {
        assert_no_file_contains(&dir, &[OPENAI_KEY, GEMINI_KEY, SIGNATURE]);
    }
}
