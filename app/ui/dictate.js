// Dictation into Yap's own window. The backend hands the transcript to the
// page instead of typing it through the OS (see own_window.rs); this inserts
// it at the cursor and replies with `own_window_reply`.
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  // Plain text inputs only: no password, URL, key-capture or other fields.
  const TEXT_INPUTS = new Set(['text', 'search']);
  let last = null; // { id, field, before, after } for undo

  // Recent focus changes, sent to the log when an insert fails so we can
  // see what took focus from the field. Element names only, never text.
  const focusLog = [];
  const name = el => !el ? 'nothing' : el === window ? 'window'
    : `${el.tagName?.toLowerCase() ?? '?'}${el.id ? '#' + el.id : ''}`;
  const note = what => {
    const t = new Date(); // local time, like yap.log
    focusLog.push(`${t.toTimeString().slice(0, 8)}.${String(t.getMilliseconds()).padStart(3, '0')} ${what}`);
    if (focusLog.length > 12) focusLog.shift();
  };
  window.addEventListener('blur', () => note('window blur'));
  window.addEventListener('focus', () => note('window focus'));
  document.addEventListener('focusin', e => note(`focus in ${name(e.target)}`));
  document.addEventListener('focusout', e => note(`focus out ${name(e.target)} -> ${name(e.relatedTarget)}`));
  document.addEventListener('visibilitychange', () => note(`visibility ${document.visibilityState}`));

  function focusedField() {
    if (!document.hasFocus()) return [null, 'Click into a text box in Yap first', 'page has no focus'];
    const field = document.activeElement;
    const editable = field instanceof HTMLTextAreaElement
      || (field instanceof HTMLInputElement && TEXT_INPUTS.has(field.type));
    if (!editable || field.disabled || field.readOnly || field.closest('[data-no-dictation]')) {
      return [null, 'Click into a text box in Yap first', `focused element is ${name(field)}`];
    }
    return [field, null, null];
  }

  function changed(field) {
    field.dispatchEvent(new Event('input', { bubbles: true }));
  }

  // Yap gives this page focus just before sending the text; let that land.
  function pageFocus(limit = 400) {
    if (document.hasFocus()) return Promise.resolve();
    return new Promise(resolve => {
      const done = () => { window.removeEventListener('focus', done); clearTimeout(timer); resolve(); };
      const timer = setTimeout(done, limit);
      window.addEventListener('focus', done);
    });
  }

  listen('dictation-insert', async ({ payload: { id, text } }) => {
    await pageFocus();
    const [field, error, why] = focusedField();
    if (field) {
      const before = field.value;
      const start = field.selectionStart ?? before.length;
      const end = field.selectionEnd ?? start;
      field.setRangeText(text, start, end, 'end');
      last = { id, field, before, after: field.value };
      changed(field);
    }
    const detail = error ? `${why}; recent focus: ${focusLog.join(' | ') || 'none'}` : null;
    invoke('own_window_reply', { id, error, detail }).catch(() => {});
  });

  listen('dictation-undo', ({ payload: { id } }) => {
    let error = 'Could not undo — the text changed';
    if (last && last.id === id && last.field.isConnected && last.field.value === last.after) {
      last.field.value = last.before;
      last.field.focus();
      changed(last.field);
      error = null;
    }
    last = null;
    invoke('own_window_reply', { id, error }).catch(() => {});
  });
})();
