// On-device model catalog and downloads, shared by Settings and onboarding.
window.Models = (() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const FAMILY_NAMES = { parakeet: 'Parakeet', whisper: 'Whisper', moonshine: 'Moonshine', nemotron: 'Nemotron' };

  // One download runs at a time. `active` is { id, pct } while it runs.
  // `onUpdate(id)` follows its progress; `onFinish(payload)` gets the
  // final event (state 'ready', 'cancelled' or 'error').
  function downloads({ onUpdate, onFinish }) {
    const tracker = {
      active: null,
      async start(id, command = 'download_model', args = { id }) {
        tracker.active = { id, pct: 0 };
        onUpdate(id);
        try {
          await invoke(command, args);
        } catch (error) {
          tracker.active = null;
          onUpdate(id);
          throw error;
        }
      },
      cancel: id => invoke('cancel_model_download', { id }),
    };
    listen('model-download-progress', ({ payload }) => {
      if (payload.state === 'downloading') {
        tracker.active = { id: payload.id, pct: Math.floor(payload.downloaded * 100 / payload.total) };
        onUpdate(payload.id);
      } else {
        tracker.active = null;
        onFinish(payload);
      }
    });
    return tracker;
  }

  return {
    catalog: () => invoke('catalog'),
    // "Parakeet TDT 0.6B v3", as the tray and history show it.
    name: model => `${FAMILY_NAMES[model.family]} ${model.label}`,
    formatBytes: bytes => bytes >= 1e9 ? (bytes / 1e9).toFixed(1) + ' GB' : Math.round(bytes / 1e6) + ' MB',
    downloads,
  };
})();
