//! Clean-up: a small local language model that tidies a transcript before it
//! is inserted — removes filler words, fixes grammar and punctuation, and
//! turns spoken lists into bullet points.
//!
//! The model runs in llama.cpp's `llama-server`, started by Yap as a helper
//! process on 127.0.0.1 with a random API key. It is a separate process
//! because linking llama.cpp into Yap would clash with whisper.cpp (both
//! bundle their own copy of ggml). Both the runtime and the models are
//! downloaded with a pinned size and SHA-256.
//!
//! This module holds the catalog, prompt and response handling; downloading
//! is in `download.rs`. Starting and stopping the server lives in the app
//! (`assistant.rs`).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::download;

/// llama.cpp build b11763 (2026-10-20), Windows x64 CPU.
pub const RUNTIME_VERSION: &str = "b11763";
const RUNTIME_FILE: &str = "llama-b11763-bin-win-cpu-x64.zip";
const RUNTIME_BYTES: u64 = 23_958_528;
const RUNTIME_SHA256: &str = "a24504506cd03a352f4990c5f3cbff347eff3a2cf7ec15038835aa690b73d0ad";
pub const SERVER_EXE: &str = "llama-server.exe";

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupModel {
    pub id: &'static str,
    pub label: &'static str,
    pub note: &'static str,
    pub filename: &'static str,
    pub bytes: u64,
    #[serde(skip)]
    pub sha256: &'static str,
    #[serde(skip)]
    pub repo: &'static str,
    #[serde(skip)]
    pub revision: &'static str,
    #[serde(skip)]
    pub format: Format,
}

/// The input each model was trained on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    /// S1-mini's fixed system prompt, then a control line above the transcript.
    S1Mini,
    /// Utter's fixed system prompt, then the bare transcript.
    Utter,
    /// The bare transcript, with no system prompt.
    Bare,
}

pub const DEFAULT_MODEL: &str = "s1-mini";

pub const MODELS: [CleanupModel; 3] = [
    CleanupModel {
        id: "s1-mini",
        label: "S1-mini by Superwhisper",
        note: "Smallest and fastest · English only",
        filename: "s1-mini-q4_k_m.gguf",
        bytes: 484_219_808,
        sha256: "3b41ebe2502cbd03e811d5d16b022f5ab551eda58d62597d152f89535003c634",
        repo: "superwhisper/s1-mini-GGUF",
        revision: "34add00a48a2e5d24e5a4ee5405a99620a3a240c",
        format: Format::S1Mini,
    },
    CleanupModel {
        id: "utter-qwen3-1.7b",
        label: "Utter dictation cleanup",
        note: "English and Hindi · follows spoken commands like “new line”",
        filename: "utter-dictation-cleanup-qwen-q6_k.gguf",
        bytes: 1_417_754_272,
        sha256: "be8d99f4659337227885b1708d4a0f0a64f634ca061fe8e2d445ffe49c3b1b0d",
        repo: "hpal007/utter-dictation-cleanup-GGUF",
        revision: "252e4ff977a38712d1ca0b4c797c2466a946858f",
        format: Format::Utter,
    },
    CleanupModel {
        id: "dictation-cleanup-qwen3.5-4b",
        label: "Dictation Cleanup (Qwen3.5 4B)",
        note: "Largest · English · slower, needs more memory",
        filename: "Qwen3.5-4B.Q4_K_M.gguf",
        bytes: 2_783_446_880,
        sha256: "d909820f5e6e42b5f3623763f70babdc93ee33aa8f6677ad3ea182f05f3f40dd",
        repo: "hasanchadaram/dictation-cleanup-qwen3.5-4b-gguf",
        revision: "d2fd182289b51652bad5c0b9f1e1118343f152bf",
        format: Format::Bare,
    },
];

pub fn model(id: &str) -> Option<&'static CleanupModel> {
    MODELS.iter().find(|model| model.id == id)
}

/// `<models>/cleanup`, holding the runtime folder and the GGUF files.
pub fn directory(models_dir: &Path) -> PathBuf {
    models_dir.join("cleanup")
}

pub fn runtime_dir(models_dir: &Path) -> PathBuf {
    directory(models_dir).join(format!("llama-{RUNTIME_VERSION}"))
}

pub fn model_path(model: &CleanupModel, models_dir: &Path) -> PathBuf {
    directory(models_dir).join(model.filename)
}

pub fn runtime_installed(models_dir: &Path) -> bool {
    let folder = runtime_dir(models_dir);
    download::unpacked(&folder) && folder.join(SERVER_EXE).is_file()
}

pub fn model_installed(model: &CleanupModel, models_dir: &Path) -> bool {
    fs::metadata(model_path(model, models_dir))
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == model.bytes)
}

/// Ready to run: runtime and model both downloaded.
pub fn installed(model: &CleanupModel, models_dir: &Path) -> bool {
    runtime_installed(models_dir) && model_installed(model, models_dir)
}

/// Bytes still to download for `model` (runtime included if missing).
pub fn download_size(model: &CleanupModel, models_dir: &Path) -> u64 {
    let runtime = if runtime_installed(models_dir) {
        0
    } else {
        RUNTIME_BYTES
    };
    let weights = if model_installed(model, models_dir) {
        0
    } else {
        model.bytes
    };
    runtime + weights
}

/// Files kept from the llama.cpp release: the server and every DLL it may
/// load (ggml picks the CPU-specific backend DLL at run time).
fn keep_runtime_file(name: &str) -> bool {
    name == SERVER_EXE || name.to_ascii_lowercase().ends_with(".dll") || name.starts_with("LICENSE")
}

/// Downloads whatever `model` still needs (runtime, weights). `progress`
/// gets the total bytes downloaded so far across both.
pub fn download(
    model: &CleanupModel,
    models_dir: &Path,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    let folder = directory(models_dir);
    fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    let mut done = 0_u64;

    if !runtime_installed(models_dir) {
        let partial = folder.join(format!("{RUNTIME_FILE}.part"));
        let url = format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/{RUNTIME_VERSION}/{RUNTIME_FILE}"
        );
        let result = download::fetch(
            &url,
            &partial,
            RUNTIME_BYTES,
            RUNTIME_SHA256,
            cancelled,
            &mut |bytes| progress(bytes),
        )
        .and_then(|()| {
            download::unpack_zip(
                &partial,
                &runtime_dir(models_dir),
                keep_runtime_file,
                SERVER_EXE,
            )
        });
        let _ = fs::remove_file(&partial);
        result?;
        done = RUNTIME_BYTES;
    }

    if !model_installed(model, models_dir) {
        let partial = folder.join(format!("{}.part", model.filename));
        let url = format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            model.repo, model.revision, model.filename
        );
        let result = download::fetch(
            &url,
            &partial,
            model.bytes,
            model.sha256,
            cancelled,
            &mut |bytes| progress(done + bytes),
        )
        .and_then(|()| {
            fs::rename(&partial, model_path(model, models_dir)).map_err(|error| error.to_string())
        });
        if result.is_err() {
            let _ = fs::remove_file(&partial);
        }
        result?;
    }
    Ok(())
}

/// Weights of clean-up models Yap no longer offers.
const RETIRED_FILES: [&str; 2] = ["Qwen3.5-2B-Q4_K_M.gguf", "Qwen3.5-4B-Q4_K_M.gguf"];

/// Deletes downloads of retired models, which Settings can no longer show.
pub fn remove_retired(models_dir: &Path) {
    for name in RETIRED_FILES {
        let _ = fs::remove_file(directory(models_dir).join(name));
    }
}

/// Removes a model's weights. The shared runtime stays.
pub fn remove(model: &CleanupModel, models_dir: &Path) -> Result<(), String> {
    match fs::remove_file(model_path(model, models_dir)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

// ---------- prompt and response ----------

/// S1-mini's required system prompt, sent byte for byte.
const S1_SYSTEM_PROMPT: &str = "You are a text normalizer for speech-to-text transcripts. The input begins with a control line specifying the styling, structure, and context settings; clean the transcript to match those settings and output only the cleaned text.";

/// Utter's required system prompt, sent byte for byte (final newline included).
const UTTER_SYSTEM_PROMPT: &str = "You clean up raw speech-to-text transcripts into written text that says exactly what the speaker meant.\n\
Rules:\n\
- Remove fillers (um, uh, like, you know, and a sentence-opening so, toh or matlab). Keep the speaker's other words, including \"basically\", \"actually\" and \"I mean\".\n\
- When the speaker restarts or corrects themselves, keep only what they landed on. Keep a self-clarification.\n\
- Fix the grammar of Latin-script English: agreement, missing small words, word forms, standard word order. Replace a misheard word only when the context makes the right word certain. Do not reword a sentence that is already correct.\n\
- Fix the casing of names (api -> API, postgres -> Postgres). Never expand them.\n\
- Keep every word in the script it came in: never rewrite Devanagari in Latin letters or Latin in Devanagari. Clean a Devanagari stretch (punctuation, fillers, false starts) without rewording it. A sentence ending in a Devanagari word ends in \"।\".\n\
- Numbers: counts under ten as words; digits for 10 and above, money, units, time, dates, percentages, versions and labels: 3 pm, ₹1,748, $10, 2%, v2. Use a currency symbol only when the speaker said dollars or rupees. Hindi times stay as spoken (3 baje).\n\
- Apply spoken commands: \"new line\", \"new paragraph\", \"open paren\".\n\
- Two or more items become a list even when the speaker never says numbers: a walk-through of topics, parallel sentences built the same way, or counted items. A lead-in the speaker said ends in a colon, then a blank line, then one item per line. Use \"1.\" \"2.\" when the speaker counted the items out loud; otherwise \"- \". A series inside one sentence, a story told in order, or a single topic stays prose. Text after the list continues as a normal paragraph.\n\
- Do not add or drop content, change the meaning, correct facts or summarize: a long dictation stays as long as it was spoken. Already-clean text comes back unchanged.\n\
Output only the cleaned text.\n";

/// S1-mini's register, from casual (lowercase, no final period) to formal
/// (contractions expanded).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Styling {
    Casual,
    SemiCasual,
    #[default]
    SemiFormal,
    Formal,
}

impl Styling {
    fn name(self) -> &'static str {
        match self {
            Self::Casual => "casual",
            Self::SemiCasual => "semi-casual",
            Self::SemiFormal => "semi-formal",
            Self::Formal => "formal",
        }
    }
}

/// S1-mini's settings. Other models ignore them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Style {
    pub styling: Styling,
    pub lists: bool,
    pub email: bool,
}

fn control_line(style: Style) -> String {
    format!(
        "[Styling: {}] [Structure: {}] [Context: {}]",
        style.styling.name(),
        if style.lists { "lists" } else { "prose" },
        if style.email { "email" } else { "general" }
    )
}

/// Dictations shorter than this are inserted as they are: there is little
/// to fix and the round trip would only add delay.
const MIN_WORDS: usize = 4;

pub fn worth_cleaning(text: &str) -> bool {
    text.split_whitespace().count() >= MIN_WORDS
}

/// The OpenAI-style chat request sent to `llama-server`.
pub fn request_body(model: &CleanupModel, style: Style, text: &str) -> serde_json::Value {
    let text = text.trim();
    // Roughly 4 characters per token, plus room for list formatting.
    let max_tokens = (text.len() / 3 + 64).min(2_048);
    let messages = match model.format {
        Format::S1Mini => serde_json::json!([
            { "role": "system", "content": S1_SYSTEM_PROMPT },
            { "role": "user", "content": format!("{}\n{text}", control_line(style)) }
        ]),
        Format::Utter => serde_json::json!([
            { "role": "system", "content": UTTER_SYSTEM_PROMPT },
            { "role": "user", "content": text }
        ]),
        Format::Bare => serde_json::json!([{ "role": "user", "content": text }]),
    };
    serde_json::json!({
        "model": "yap-cleanup",
        "messages": messages,
        "temperature": 0,
        "max_tokens": max_tokens,
        "stream": false,
        // All three were trained with thinking off.
        "chat_template_kwargs": { "enable_thinking": false }
    })
}

/// Pulls the cleaned text out of a chat response. Returns `None` when it is
/// missing or empty.
pub fn parse_response(body: &serde_json::Value) -> Option<String> {
    let content = body.pointer("/choices/0/message/content")?.as_str()?;
    let mut text = content.to_owned();
    // Drop any reasoning block the model emitted anyway.
    while let (Some(start), Some(end)) = (text.find("<think>"), text.find("</think>")) {
        if end < start {
            break;
        }
        text.replace_range(start..end + "</think>".len(), "");
    }
    let text = text
        .replace("<dictation>", "")
        .replace("</dictation>", "")
        .trim()
        .to_owned();
    (!text.is_empty()).then_some(text)
}

/// Sends `text` to a running clean-up server.
pub fn clean(
    base_url: &str,
    api_key: &str,
    model: &CleanupModel,
    style: Style,
    text: &str,
    timeout: Duration,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|_| "Could not start clean-up".to_owned())?;
    let response = client
        .post(format!(
            "{}/v1/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(api_key)
        .header("Content-Type", "application/json")
        .body(request_body(model, style, text).to_string())
        .send()
        .map_err(|_| "Clean-up did not respond in time".to_owned())?;
    if !response.status().is_success() {
        return Err(format!("Clean-up failed ({})", response.status()));
    }
    let mut body = Vec::new();
    response
        .take(1_048_576)
        .read_to_end(&mut body)
        .map_err(|_| "Could not read the clean-up result".to_owned())?;
    let body: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| "Invalid clean-up result".to_owned())?;
    parse_response(&body).ok_or_else(|| "Clean-up returned no text".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::net::TcpListener;

    fn reply(content: &str) -> serde_json::Value {
        serde_json::json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
    }

    #[test]
    fn catalog_is_pinned() {
        for model in &MODELS {
            assert_eq!(model.sha256.len(), 64);
            assert!(model.filename.ends_with(".gguf"));
            assert_eq!(super::model(model.id).unwrap().filename, model.filename);
            assert_eq!(model.revision.len(), 40);
        }
        assert!(model(DEFAULT_MODEL).is_some());
        assert_eq!(RUNTIME_SHA256.len(), 64);
        assert!(model("../x").is_none());
    }

    const STYLE: Style = Style {
        styling: Styling::SemiFormal,
        lists: true,
        email: false,
    };

    #[test]
    fn s1_mini_gets_its_prompt_and_control_line() {
        let body = request_body(model("s1-mini").unwrap(), STYLE, " um so we should go ");
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["messages"][0]["content"], S1_SYSTEM_PROMPT);
        assert_eq!(
            body["messages"][1]["content"],
            "[Styling: semi-formal] [Structure: lists] [Context: general]\num so we should go"
        );
        assert!(body["max_tokens"].as_u64().unwrap() >= 64);
        let casual = Style {
            styling: Styling::Casual,
            lists: false,
            email: true,
        };
        let body = request_body(model("s1-mini").unwrap(), casual, "hi");
        assert_eq!(
            body["messages"][1]["content"],
            "[Styling: casual] [Structure: prose] [Context: email]\nhi"
        );
    }

    #[test]
    fn utter_gets_its_exact_prompt_and_the_bare_transcript() {
        use sha2::{Digest, Sha256};
        let hash = format!("{:x}", Sha256::digest(UTTER_SYSTEM_PROMPT.as_bytes()));
        assert_eq!(
            hash,
            "82a71e66556e6d809411bdd7853abeb84e6856c5bf12dc5e008bb81a37b36f99"
        );
        let body = request_body(model("utter-qwen3-1.7b").unwrap(), STYLE, "um hi there");
        assert_eq!(body["messages"][0]["content"], UTTER_SYSTEM_PROMPT);
        assert_eq!(body["messages"][1]["content"], "um hi there");
    }

    #[test]
    fn dictation_cleanup_gets_only_the_transcript() {
        let body = request_body(
            model("dictation-cleanup-qwen3.5-4b").unwrap(),
            STYLE,
            "um hi there",
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "um hi there");
    }

    #[test]
    fn accepts_a_cleaned_list_and_strips_reasoning_and_tags() {
        let cleaned = parse_response(
            &reply("<think>\nok\n</think>\n<dictation>For the trip we need:\n- Sunscreen\n- Towels\n- Snacks</dictation>"),
        )
        .unwrap();
        assert_eq!(
            cleaned,
            "For the trip we need:\n- Sunscreen\n- Towels\n- Snacks"
        );
    }

    #[test]
    fn returns_any_text_and_rejects_only_empty_replies() {
        assert_eq!(parse_response(&reply("Paris.")).as_deref(), Some("Paris."));
        assert!(parse_response(&reply("")).is_none());
        assert!(parse_response(&reply("<dictation> </dictation>")).is_none());
        assert!(parse_response(&serde_json::json!({})).is_none());
    }

    #[test]
    fn short_dictations_are_left_alone() {
        assert!(!worth_cleaning("Okay thanks"));
        assert!(worth_cleaning("Okay thanks see you tomorrow"));
    }

    #[test]
    fn removes_only_retired_models() {
        let root = std::env::temp_dir().join(format!("yap-retired-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = directory(&root);
        fs::create_dir_all(&folder).unwrap();
        for name in RETIRED_FILES.iter().chain([&MODELS[0].filename]) {
            fs::write(folder.join(name), b"x").unwrap();
        }
        remove_retired(&root);
        remove_retired(&root);
        assert!(RETIRED_FILES.iter().all(|name| !folder.join(name).exists()));
        assert!(folder.join(MODELS[0].filename).exists());
        assert!(
            MODELS
                .iter()
                .all(|model| !RETIRED_FILES.contains(&model.filename))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keeps_only_runtime_files_it_needs() {
        assert!(keep_runtime_file("llama-server.exe"));
        assert!(keep_runtime_file("ggml-cpu-haswell.dll"));
        assert!(keep_runtime_file("LICENSE-LLVM-OpenMP"));
        assert!(!keep_runtime_file("llama-cli.exe"));
    }

    #[test]
    fn unpacks_server_and_dlls_from_the_release_zip() {
        let root = std::env::temp_dir().join(format!("yap-runtime-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let archive = root.join("llama.zip");
        {
            let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            for name in [
                "llama-server.exe",
                "llama.dll",
                "llama-cli.exe",
                "sub/../ggml.dll",
            ] {
                zip.start_file(name, options).unwrap();
                zip.write_all(b"x").unwrap();
            }
            zip.finish().unwrap();
        }
        let destination = runtime_dir(&root);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        download::unpack_zip(&archive, &destination, keep_runtime_file, SERVER_EXE).unwrap();
        assert!(runtime_installed(&root));
        assert!(destination.join("llama.dll").is_file());
        assert!(destination.join("ggml.dll").is_file());
        assert!(!destination.join("llama-cli.exe").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clean_posts_to_the_local_server_with_the_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 8192];
            loop {
                let count = connection.read(&mut chunk).unwrap();
                request.extend_from_slice(&chunk[..count]);
                let text = String::from_utf8_lossy(&request).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text
                        .to_ascii_lowercase()
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: ").map(str::to_owned))
                        .unwrap()
                        .trim()
                        .parse::<usize>()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let body = serde_json::to_vec(&reply("We should go on Wednesday.")).unwrap();
            write!(
                connection,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .unwrap();
            connection.write_all(&body).unwrap();
            String::from_utf8_lossy(&request).to_string()
        });
        let cleaned = clean(
            &base,
            "secret-key",
            model(DEFAULT_MODEL).unwrap(),
            STYLE,
            "um we should go on tuesday no wednesday",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(cleaned, "We should go on Wednesday.");
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /v1/chat/completions"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer secret-key")
        );
    }
}
