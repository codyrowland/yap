// API keys for cloud providers ('cloud', provider id) and the self-hosted
// server ('remote', no provider), shared by Settings and onboarding.
window.Credentials = (() => {
  const { invoke } = window.__TAURI__.core;
  return {
    saved: (kind, provider = '') => invoke('credential_status', { kind, provider }),
    save: (kind, provider, secret) => invoke('update_credential', { kind, provider, secret }),
    remove: (kind, provider = '') => invoke('update_credential', { kind, provider, secret: null }),
  };
})();
