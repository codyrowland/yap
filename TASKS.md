# Yap tasks

The product spec is **flow 2 (2a–2i)** on the Yap mockups canvas (Claude Design project "Yap", page "Hush Mockups"): onboarding, Settings (talk keys, on-device, cloud, self-hosted, general), History, recording overlay and tray popover. The older Hush round has been removed from the canvas. Mockup interactions must be backed by real app behavior; anything not yet real is shown disabled ("Coming soon").

Next: run the new build on Windows and walk 2a–2i → protocol adapters for the remaining cloud providers → other on-device model families.

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
- [ ] Other on-device families (Parakeet, Canary, Moonshine, Qwen3-ASR, Voxtral, Kyutai, Vosk, SenseVoice, Granite) — shown as "Coming soon". Suggest starting with one shared runtime (e.g. sherpa-onnx) for Parakeet/Moonshine/SenseVoice.

## Onboarding and input

- [x] Five-step onboarding with real checks; engine step offers on-device, cloud (supported providers) and self-hosted.
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
- [ ] Optional live partial text in the overlay.

## History

- [x] Persisted insertions for the current day; search, app filter, copy, delete.

## Reliability

- [x] Request timeouts (90 s per upload), response-size limits, cancellation; late results from cancelled jobs are never inserted.
- [ ] Integration test for settings migration on a real Windows profile, and the input harness against paste insertion.
- [ ] Remove the interim environment-variable credential fallback once existing configs have migrated.
