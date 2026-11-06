# Yap tasks

The product spec is **flow 2 (2a–2i)** on the Yap mockups canvas (Claude Design project "Yap", page "Hush Mockups"): onboarding, Settings (talk keys, on-device, cloud, self-hosted, general), History, recording overlay and tray popover. The older Hush round has been removed from the canvas. Mockup interactions must be backed by real app behavior; anything not yet real is shown disabled ("Coming soon").

Next: architecture clean-up (below) → platform interface refactor and OS-neutral key names (see Cross-platform) → Windows release items (bundle the sherpa-onnx DLLs and VC++ runtime, test clean-up with real models) → macOS port → Linux.

## Structure

- [x] Settings stored as JSON (`settings.json`, serde) with a one-time import of the old line-based `settings.txt` (v1–v5). Atomic writes.
- [x] Split the 1,800-line `main.rs` into `config`, `state`, `dictation`, `overlay`, `tray`, `onboarding`, `commands`, `clipboard`, `autostart`, `input_test`.
- [x] Cross-check and unit tests run from Linux: `cargo check/test --target x86_64-pc-windows-gnu --no-default-features` (tests under Wine). whisper.cpp itself still needs a Windows build.
- [x] Removed the leftover root crate `yap-input-probe` (`/Cargo.toml`, `/src`), superseded by `app/src-tauri/src/bin/input_harness.rs`.

## Provider support

- [x] Local whisper.cpp, OpenAI-compatible self-hosted server, explicit engine selection snapshotted per job.
- [x] Cloud provider registry with the 11 designed providers. Working today (OpenAI protocol): OpenAI, Groq, Fireworks AI, Custom OpenAI-compatible API. Listed but disabled: Deepgram, AssemblyAI, ElevenLabs, Google Cloud, Azure, AWS Transcribe, Speechmatics.
- [ ] Protocol-specific adapters for the disabled providers (Google service-account file, AWS SigV4, Azure region keys, etc.).
- [x] Per-provider keys in Windows Credential Manager; the custom API's key is bound to its base URL; self-hosted token bound to the server URL.
- [x] Real "Test connection" for cloud and self-hosted (uploads one second of silence; reports latency or the error).
- [x] Whisper model catalog: verified download/progress/cancel, select, delete, storage usage, open folder.

## On-device models

Decision (Nov 2026): ship four families, not ten. Whisper for breadth (99 languages), Parakeet as the fast default, Moonshine for low-end PCs, Nemotron Streaming for live text. The last three share one runtime (sherpa-onnx). Dropped from the design: Canary, Qwen3-ASR, Voxtral, Granite (1–24B parameters, need GPU-class hardware for about one point better WER), Kyutai (own runtime), Vosk (older, less accurate), SenseVoice (CJK only; easy to add later on sherpa-onnx if needed).

- [x] sherpa-onnx runtime (`sherpa-onnx` crate 1.13.8, `shared` feature, behind the `local-sherpa` cargo feature, on by default). Shared DLLs avoid the /MT vs /MD CRT clash with whisper.cpp; the crate's build script downloads them from GitHub and copies them next to the exe.
- [x] Parakeet TDT 0.6B v3 (multilingual, recommended) and v2 (English): pinned size + SHA-256, download `.tar.bz2` from the sherpa-onnx `asr-models` release, unpack only encoder/decoder/joiner/tokens into `models/<id>/`. v3 archive layout and config checked against sherpa-onnx's Python build (en/de/fr samples transcribed correctly).
- [x] Loaded on-device model is cached between dictations and preloaded on start and when the model changes (loading takes ~2 s).
- [x] Settings › On-device and onboarding offer Parakeet (onboarding defaults to v3).
- [ ] Bundle `sherpa-onnx-c-api.dll` and `onnxruntime.dll` with the installer (Tauri `bundle.resources`); `cargo tauri dev` already finds them.
- [x] Parakeet v2 archive layout verified (same file names as v3).
- [x] Moonshine v2 tiny and base (English; `encoder_model.ort` + `decoder_model_merged.ort`), offline like Parakeet.
- [x] Nemotron streaming (560 ms chunks): Speech Streaming EN 0.6B and 3.5 ASR Streaming 0.6B (multilingual; gets the spoken-language setting as a per-stream option, "auto" by default). Audio is fed to the model while recording; partial text shows in the overlay; the final text is ready shortly after release. If the model is still loading at release, the whole recording is transcribed in one go instead.
- [x] Half a second of lead-in silence for streaming: without it Nemotron 3.5 dropped the first word on sherpa-onnx's own sample clips.
- [x] All four archives checked: pinned SHA-256 matches GitHub's digests, file layout matches, and each model transcribed sherpa-onnx's sample clips correctly through the Python build of sherpa-onnx 1.13.8 with the same settings Yap uses.
- [ ] Offer the other Nemotron chunk sizes (80/160/1120 ms) if 560 ms feels laggy or inaccurate.
- [ ] Moonshine models for other languages (es, zh, ja, ko, ar, uk, vi) — same format, just catalog entries.
- [ ] Show installed size (Parakeet unpacks to ~670 MB; the UI shows the 487 MB download size).

## Clean-up (local language model)

Removes "um"s and false starts, fixes grammar and punctuation, turns spoken lists into bullet points. Off by default; turned on in Settings › General after downloading a model.

- [x] Runs llama.cpp's `llama-server` (build b11763, pinned SHA-256) as a helper process on 127.0.0.1 with a random API key, in a Windows job object so it dies with Yap. A separate process because linking llama.cpp would clash with whisper.cpp's copy of ggml.
- [x] Models: Qwen3.5 2B (default) and 4B, Q4_K_M GGUFs from Hugging Face, pinned size + SHA-256. Runtime (24 MB) downloads with the first model.
- [x] Thinking disabled (`--reasoning-budget 0` + `enable_thinking: false`); low temperature; dictation wrapped in tags and the model told not to follow instructions inside it.
- [x] Safety: dictations under 4 words skip clean-up; a reply much shorter or longer than the dictation (an answer instead of an edit, a truncation) is discarded; any error or 15 s+ timeout inserts the original words and says so.
- [x] Overlay shows "Cleaning up"; history stores the cleaned text.
- [ ] Not yet run with the real models (Hugging Face is blocked from the build sandbox): check quality and speed on Windows, tune the prompt.
- [ ] llama-server needs the Microsoft Visual C++ Redistributable (x64); Yap reports it if missing. Ship it with the installer.
- [ ] Bullets with "Simulated typing" insertion depend on the target app handling newlines; paste works everywhere.

## Onboarding and input

- [x] Five-step onboarding with real checks; engine step offers on-device, cloud (supported providers) and self-hosted.
- [x] "Try it" (step 5) is a plain text box filled by normal dictation: no on-screen record button and no separate practice path. Finishing after a dictation no longer fails with "while dictating".
- [x] Talk key works while a Yap window is in front. Tauri registered raw keyboard input on Yap's windows, and Windows then skipped Yap's own keyboard hook while Yap was focused (wry #1761, tauri #13919). Fixed with `device_event_filter(DeviceEventFilter::Always)`; Yap doesn't use device events.
- [x] Dictating into Yap's own window (onboarding, History search, Settings fields): the transcript is handed to the page (`own_window.rs`, `ui/dictate.js`), which inserts it at the cursor, instead of being typed through the OS. Only textareas and text/search inputs; never password, URL or key fields, or anything marked `data-no-dictation`. Undo works there too. Needs no platform code.
- [x] The overlay never keeps focus: shown with `SW_SHOWNOACTIVATE`, and if its WebView2 page still takes focus when it appears, activation goes straight back to the main window (`overlay::give_back_focus`). This only ever affected dictating into Yap itself.
- [ ] Windows end-to-end harness test for dictating into Yap's own window (needs the window in front).
- [x] Separate hold and toggle keys with sequential capture, conflicts, reset, Esc cancel.
- [x] Insertion method: paste with clipboard restore (falls back to typing when the clipboard holds non-text; skips clipboard history) or simulated typing.
- [x] Spoken language (auto, en, de, es, fr) passed to whisper.cpp and OpenAI-protocol providers.
- [x] Open at login (HKCU Run key, starts hidden in the tray).
- [ ] "Start and stop sounds" toggle from the design (not built; omitted from General).

## Overlay and tray (2h, 2i)

- [x] Overlay window at the bottom of the active monitor: live level meter + timer, hold vs toggle (Stop button), transcribing with engine/model, "Inserted N words into App" with Undo (Ctrl+Z if the same field still has focus), errors.
- [x] Recordings up to 5 minutes; hitting the limit stops and transcribes instead of discarding.
- [x] Tray popover on left-click: status, talk keys, engine switcher, History…, Settings…, Quit. Right-click keeps a native menu.
- [ ] Friendlier app names in "Inserted into …" (currently the process name, e.g. OUTLOOK).
- [x] Live partial text in the overlay (Nemotron streaming models).

## History

- [x] Persisted insertions for the current day; search, app filter, copy, delete.

## Reliability

- [x] Request timeouts (90 s per upload), response-size limits, cancellation; late results from cancelled jobs are never inserted.
- [x] `yap.log` in the app data folder (timestamped, rotated at 1 MB, never transcripts or credentials): talk keys, each dictation step (recording length, word count, where it was inserted or why not), settings saves, clean-up status. Debug builds also log talk-key hook activity.
- [ ] Integration test for settings migration on a real Windows profile, and the input harness against paste insertion.
- [ ] Remove the interim environment-variable credential fallback once existing configs have migrated.

## Architecture clean-up

Review (Nov 2026): the core library (coordinator, speech engines, settings, history, catalogs) is in good shape; the app layer grew in place while fixing bugs. Do these before the platform refactor, in this order, since they touch the same files. Each step keeps Windows behaving the same and is checked with the build, the tests and a short dictation round on Windows.

- [ ] Coordinator owns the whole job. `Session` duplicates it (`active_id`, `target`, `cancelled`, `history_meta`, `last_insert`) and keeps the copies in sync by hand, which caused the onboarding job stuck in Inserting; `active_id != Some(id)`-style checks appear about 40 times.
- [ ] Settings get their own lock (or are swapped in whole), separate from the dictation job. Reading a setting currently locks the whole session; that caused the OpenAI freeze, and `overlay::enabled` still needs `try_lock` to avoid it.
- [ ] Split `dictation.rs` (~960 lines: engine cache, live streaming, recording, transcription, clean-up, both insertion paths, history saving, overlay updates, talk-key event loop) into a short pipeline plus engines, live, insert and history modules.
- [ ] Split `hotkey.rs` (~960 lines): key names and parsing, the hold/toggle state machine and key capture are portable and move to the library with tests; the Windows hook and the debug trace move to the Windows platform code.
- [ ] Gather the Windows code now spread over `hotkey`, `platform_input`, `clipboard`, `autostart`, `assistant`, `overlay`, `input_test`, the harness and `credential_store` behind the platform interface (see Cross-platform). `credential_store` is Windows-only but sits in the "OS-independent" library.
- [ ] Remove test branches from production code: 13 `#[cfg(feature = "input-test")]` blocks in `begin_recording`, `stop_recording`, `audio`, `state` and `main`. Use a fake microphone and a fake speech engine through the existing interfaces instead.
- [ ] Split `commands.rs` (~600 lines, every screen's commands) by area: settings, models, credentials, clean-up, history, tray.
- [ ] Separate catalogs from downloading: `models.rs` and `cleanup.rs` each mix a catalog with download/verify/unpack code; move that into one small download module.
- [ ] UI: onboarding and Settings each have their own copy of key capture, model download, credential and catalog code (the key-capture bug had to be fixed twice). Move it into shared scripts like `dictate.js`.
- [ ] Later: typed errors instead of plain strings.

## Cross-platform (macOS, Linux)

Decision (Nov 2026): stay on Tauri. The same HTML/CSS screens run on WebView2, WebKit (macOS) and WebKitGTK (Linux), and whisper.cpp, sherpa-onnx and llama.cpp all build for macOS (Metal) and Linux. The non-portable part is the OS layer: talk keys, insertion, the field check, permissions, autostart, credentials and the overlay window. Order: Windows release first, then macOS, then Linux.

- [x] Dictation into Yap's own window goes through the page, not the OS, so it already works the same everywhere.
- [ ] Platform interface: one module with a fixed interface for talk keys (start, update, capture, press/release events), insertion target (find, verify, insert), clipboard, permissions, autostart, credential store and the non-activating overlay. Windows becomes the first implementation; no behaviour change. The coordinator, speech, clean-up, history and settings are already OS-independent.
- [ ] OS-neutral key names in settings (W3C codes such as `AltRight`, `Fn`), migrated from the current Windows names, with each OS showing its own labels (Option/Command and fn/Globe on macOS).
- [ ] Clean-up helper process ends with Yap on every OS (job object on Windows; process group / parent-death signal on Linux; equivalent on macOS).
- [ ] Destroy the Settings/History window when closed and recreate it on open, so its WebView doesn't stay in memory; only the tray and overlay stay alive.
- [ ] macOS: talk keys via an event tap (Accessibility and Input Monitoring permissions, added to onboarding); field check and typing via the Accessibility API; paste via synthetic Cmd+V; microphone usage description; signing and notarization; non-activating panel for the overlay (a normal window takes focus from the target app); Keychain for API keys; Metal builds of whisper.cpp and llama.cpp; sherpa-onnx macOS libraries.
- [ ] Linux, X11 first: global keys and synthetic input work as on Windows. Then Wayland, which blocks global key listening and typing into other apps: use the xdg-desktop-portal GlobalShortcuts and RemoteDesktop portals (support varies by desktop; check before starting). AT-SPI field checks are inconsistent, so expect weaker guarantees (paste via clipboard, or "copied, press Ctrl+V"). Also: WebKitGTK quirks, overlay transparency and positioning on Wayland, Secret Service for API keys, tray via AppIndicator.
