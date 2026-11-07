// Talk keys, shared by Settings, onboarding and the tray. Shortcuts are
// saved with W3C key names ("Control+Space", "AltRight", "Meta+KeyL"); a
// modifier without "Right" means either side. `labels` shows them the way
// this OS names the keys.
//
// Capture: keys are pressed one at a time and saved with `complete`; the
// backend pauses the talk keys while capturing. Each page draws the capture
// itself in `onChange`.
window.KeyCapture = (() => {
  const { invoke } = window.__TAURI__.core;
  const MODIFIERS = ['Control', 'Alt', 'Shift', 'Meta'];
  const NAMED_KEYS = new Set(['Space', 'Tab', 'Enter', 'Backspace', 'Delete', 'Insert', 'Home', 'End', 'PageUp', 'PageDown', 'ArrowLeft', 'ArrowUp', 'ArrowRight', 'ArrowDown', 'Semicolon', 'Equal', 'Comma', 'Minus', 'Period', 'Slash', 'Backquote', 'BracketLeft', 'Backslash', 'BracketRight', 'Quote', 'IntlBackslash', 'CapsLock', 'NumLock', 'ScrollLock', 'Pause', 'ContextMenu', 'NumpadAdd', 'NumpadSubtract', 'NumpadMultiply', 'NumpadDivide', 'NumpadDecimal']);
  const modifierName = event => MODIFIERS.includes(event.key) ? event.key : null;

  const agent = navigator.userAgent;
  const MODIFIER_LABELS = /Mac/.test(agent) ? { Control: 'Control', Alt: 'Option', Shift: 'Shift', Meta: 'Command' }
    : /Windows/.test(agent) ? { Control: 'Ctrl', Alt: 'Alt', Shift: 'Shift', Meta: 'Win' }
    : { Control: 'Ctrl', Alt: 'Alt', Shift: 'Shift', Meta: 'Super' };
  const KEY_LABELS = { ArrowLeft: 'Left', ArrowUp: 'Up', ArrowRight: 'Right', ArrowDown: 'Down', PageUp: 'Page Up', PageDown: 'Page Down', Semicolon: ';', Equal: '=', Comma: ',', Minus: '-', Period: '.', Slash: '/', Backquote: '`', BracketLeft: '[', Backslash: '\\', BracketRight: ']', Quote: "'", IntlBackslash: '\\ (ISO)', CapsLock: 'Caps Lock', NumLock: 'Num Lock', ScrollLock: 'Scroll Lock', ContextMenu: 'Menu', NumpadAdd: 'Num +', NumpadSubtract: 'Num -', NumpadMultiply: 'Num *', NumpadDivide: 'Num /', NumpadDecimal: 'Num .' };

  function label(name) {
    const right = name.endsWith('Right') && MODIFIER_LABELS[name.slice(0, -5)];
    if (right) return `Right ${right}`;
    if (MODIFIER_LABELS[name]) return MODIFIER_LABELS[name];
    if (/^Key[A-Z]$/.test(name)) return name.slice(3);
    if (/^Digit[0-9]$/.test(name)) return name.slice(5);
    if (/^Numpad[0-9]$/.test(name)) return `Num ${name.slice(6)}`;
    return KEY_LABELS[name] || name;
  }
  const labels = shortcut => shortcut ? shortcut.split('+').map(label) : [];

  function browserKey(event) {
    const code = event.code;
    if (/^(?:Key[A-Z]|Digit[0-9]|Numpad[0-9]|F(?:[1-9]|1[0-9]|2[0-4]))$/.test(code)) return code;
    return NAMED_KEYS.has(code) ? code : null;
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
        state.modifiers.set(modifier, modifier + (event.location === 2 ? 'Right' : ''));
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
      for (const [name, pressed] of [['Control', event.ctrlKey], ['Alt', event.altKey], ['Shift', event.shiftKey], ['Meta', event.metaKey]]) {
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

  return { create, labels };
})();
