// Talk-key capture, shared by Settings and onboarding. Keys are pressed one
// at a time and saved with `complete`; the backend pauses the talk keys
// while capturing. Each page draws the capture itself in `onChange`.
window.KeyCapture = (() => {
  const { invoke } = window.__TAURI__.core;
  const MODIFIERS = ['Ctrl', 'Alt', 'Shift', 'Meta'];
  const KEY_NAMES = { Space: 'Space', Tab: 'Tab', Enter: 'Enter', Backspace: 'Backspace', Delete: 'Delete', Insert: 'Insert', Home: 'Home', End: 'End', PageUp: 'PageUp', PageDown: 'PageDown', ArrowLeft: 'Left', ArrowUp: 'Up', ArrowRight: 'Right', ArrowDown: 'Down', Semicolon: 'Semicolon', Equal: 'Equals', Comma: 'Comma', Minus: 'Minus', Period: 'Period', Slash: 'Slash', Backquote: 'Backtick', BracketLeft: 'LeftBracket', Backslash: 'Backslash', BracketRight: 'RightBracket', Quote: 'Quote' };
  const modifierName = event => ({ Control: 'Ctrl', Alt: 'Alt', Shift: 'Shift', Meta: 'Meta' })[event.key];

  function browserKey(event) {
    const code = event.code;
    if (/^Key[A-Z]$/.test(code)) return code.slice(3);
    if (/^Digit[0-9]$/.test(code)) return code.slice(5);
    if (/^F(?:[1-9]|1[0-9]|2[0-4])$/.test(code) || /^Numpad[0-9]$/.test(code)) return code;
    return KEY_NAMES[code] || null;
  }

  function create({ onChange, onError }) {
    const state = { kind: null, modifiers: new Map(), key: null, timer: null };
    const reset = () => {
      state.kind = null;
      clearTimeout(state.timer);
      state.modifiers.clear();
      state.key = null;
    };

    const capture = {
      // 'hold' | 'toggle' while capturing, otherwise null.
      get kind() { return state.kind; },
      shortcut() {
        const parts = MODIFIERS.filter(m => state.modifiers.has(m)).map(m => state.modifiers.get(m));
        if (state.key) parts.push(state.key);
        return parts.join('+');
      },
      async begin(kind) {
        if (state.kind === kind) return;
        try {
          await capture.cancel();
          state.kind = kind;
          onChange();
          await invoke('begin_key_capture', { kind });
          if (state.kind === kind) state.timer = setTimeout(() => capture.cancel().catch(onError), 58000);
        } catch (error) {
          await capture.cancel().catch(() => {});
          onError(error);
        }
      },
      async cancel() {
        if (!state.kind) return;
        reset();
        onChange();
        await invoke('cancel_key_capture');
      },
      // Returns { kind, shortcut } once saved, or null if it was rejected.
      async complete() {
        if (!state.kind) return null;
        const kind = state.kind;
        const shortcut = capture.shortcut();
        try {
          await invoke('complete_key_capture', { kind, shortcut });
        } catch (error) {
          onError(error);
          return null;
        }
        reset();
        onChange();
        return { kind, shortcut };
      },
    };

    function onKey(event) {
      if (!state.kind) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      if (event.type !== 'keydown' || event.repeat) return;
      if (event.key === 'Escape') {
        capture.cancel().catch(onError);
        return;
      }
      const modifier = modifierName(event);
      if (modifier) {
        state.modifiers.set(modifier, (event.location === 2 ? 'Right ' : '') + modifier);
        onChange();
        return;
      }
      const key = browserKey(event);
      if (!key) {
        onError('This key is not supported');
        return;
      }
      if (state.key && state.key !== key) {
        onError('A combination can have only one non-modifier key. Cancel and try again.');
        return;
      }
      for (const [name, pressed] of [['Ctrl', event.ctrlKey], ['Alt', event.altKey], ['Shift', event.shiftKey], ['Meta', event.metaKey]]) {
        if (pressed && !state.modifiers.has(name)) state.modifiers.set(name, name);
      }
      state.key = key;
      onChange();
    }

    window.addEventListener('keydown', onKey, true);
    window.addEventListener('keyup', onKey, true);
    window.addEventListener('pagehide', () => invoke('cancel_key_capture').catch(() => {}));
    return capture;
  }

  return { create };
})();
