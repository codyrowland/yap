const Theme = {
  apply(theme) {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem('yap-theme', theme);
  },
  sync() {
    return window.__TAURI__.core.invoke('current_settings').then(settings => Theme.apply(settings.theme)).catch(() => {});
  },
};
document.documentElement.dataset.theme = localStorage.getItem('yap-theme') || 'dark';
window.__TAURI__.event.listen('settings-changed', Theme.sync);
Theme.sync();
