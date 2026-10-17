# Yap tasks

The Yap Onboarding, Settings, and History screens in `C:\Users\codyr\Downloads\Hush desktop transcription app` are the product spec. `Settings v1` (Hush) is an older design, not a second spec. Mockup interactions must be backed by real app behavior.

Next: talk keys and onboarding → cloud providers → other on-device model families.

## Provider support

- [x] Make speech provider selection an explicit setting and snapshot it per dictation job. Keep local whisper.cpp working.
- [x] Migrate existing v1/v2 settings without losing local model or microphone preferences.
- [x] Add an OpenAI-compatible transcription adapter for hosted APIs and models served on another computer (JSON `text` response, WAV multipart upload, optional API key from an environment variable).
- [x] Wire provider selection, endpoint, model, and authentication environment variable through saved settings and dictation; do not store API keys in `settings.txt`.
- [x] Update the current basic UI to select local or OpenAI-compatible transcription and configure the relevant fields.
- [ ] Replace the basic UI with the Yap Settings design; make each control reflect and persist real state.
- [ ] Support each cloud provider shown in Yap Settings: OpenAI, Deepgram, AssemblyAI, Groq, ElevenLabs, Google Cloud Speech-to-Text, Azure AI Speech, AWS Transcribe, Speechmatics, Fireworks AI, and custom OpenAI-compatible API. Implement protocol-specific adapters where necessary.
- [ ] Support the separate self-hosted server settings, optional auth token, and real connection tests for cloud and self-hosted engines.
- [x] Store OpenAI-compatible endpoint credentials in Windows Credential Manager; never write keys or tokens into the settings file or history.
- [ ] Extend credential storage to each cloud provider and self-hosted configuration.
- [x] Support Whisper model catalog, verified download/progress/cancel, selection, storage usage, and opening the model folder.
- [ ] Support the mockup's other on-device model families with matching inference runtimes and verified model downloads.

## Onboarding and input

- [x] Implement first-run five-step onboarding with real microphone/UI Automation checks on Windows, supported engine setup, talk-key selection, and a live talk-key trial inside Yap; resume unfinished setup after relaunch.
- [ ] Expand onboarding choices as the remaining cloud and on-device engines become available.
- [x] Support separate hold and toggle keys, sequential capture of chord bindings (including Windows and right-side modifier keys), conflict handling, reset, and Esc cancellation on Windows; migrate existing F8–F10 bindings. Reject shortcuts reserved by Windows.
- [ ] Support the General controls for insertion method (paste with clipboard restoration or simulated typing) and spoken language.

## History

- [x] Persist successful insertions with the app, time, duration, text, engine, and model needed by Yap History; expire entries after the current local calendar day.
- [x] Implement the History screen's search, app filter, selection, copy, and deletion against persisted data.
- [x] Keep history enabled for the current local calendar day only; clear it when a new day starts or the app next opens.

## Reliability

- [ ] Define and enforce remote request timeouts, cancellation, response-size limits, and useful error reporting. Never insert late results from cancelled or superseded jobs.
- [x] Avoid holding the session mutex during audio stopping/normalization or network work.
- [ ] Test settings migration and adapter success, malformed responses, authentication errors, timeouts, and cancellation against a controllable server.

## Open decisions

- [ ] Remove the interim environment-variable credential fallback after migrating existing configurations to the system credential store.
- [x] Start with OpenAI-compatible multipart WAV upload and JSON `text` response; use optional bearer authentication from an environment variable. Other server protocols can get separate adapters later.
