# Yap tasks

## Provider support

- [ ] Make speech provider selection an explicit setting and snapshot it per dictation job. Keep local whisper.cpp working.
- [ ] Migrate existing v1/v2 settings without losing local model or microphone preferences.
- [ ] Add an OpenAI-compatible transcription adapter for both hosted APIs and models served on another computer. Configure endpoint, model, and authentication separately; do not store API keys in `settings.txt`.
- [ ] Update the UI to select local or OpenAI-compatible transcription and configure the relevant fields.

## Reliability

- [ ] Define and enforce remote request timeouts, cancellation, response-size limits, and useful error reporting. Never insert late results from cancelled or superseded jobs.
- [ ] Avoid holding the session mutex during audio stopping/normalization or network work.
- [ ] Test settings migration and adapter success, malformed responses, authentication errors, timeouts, and cancellation against a controllable server.

## Open decisions

- [ ] Choose how to store or supply API credentials securely on supported platforms.
- [ ] Decide which OpenAI-compatible transcription request/response format and authentication options to support first; other server protocols can get separate adapters later.
