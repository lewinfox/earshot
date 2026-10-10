// Keyboard → control bus. Uses physical key positions (KeyboardEvent.code), so the
// layout is the same on any keyboard language: left hand plays deck A, right hand B.
//
// Focus rule: deck keys are live unless a text field has focus. Esc always blurs the
// field and returns to deck keys.

export interface KeyBinding {
  group: string;
  key: string;
  /** "button" sends 1 on press and 0 on release; "toggle" flips on press; "value" sends `value` on press. */
  kind: "button" | "toggle" | "value";
  value?: number;
  label: string;
}

const A = "[Channel1]";
const B = "[Channel2]";

function deckKeys(g: string, keys: Record<string, string>): Record<string, KeyBinding> {
  const b = (key: string, label: string, kind: KeyBinding["kind"] = "button"): KeyBinding => ({ group: g, key, kind, label });
  return {
    [keys.cue]: b("cue_default", "cue"),
    [keys.play]: b("play", "play / pause", "toggle"),
    [keys.sync]: b("sync_enabled", "sync", "toggle"),
    [keys.pfl]: b("pfl", "headphone cue", "toggle"),
    [keys.jumpBack]: b("beatjump_backward", "beat jump back"),
    [keys.jumpFwd]: b("beatjump_forward", "beat jump forward"),
    [keys.loop]: b("beatloop_activate", "4-beat loop on/off"),
    [keys.reloop]: b("reloop_toggle", "reloop / exit"),
    [keys.slower]: b("rate_temp_down", "nudge slower"),
    [keys.faster]: b("rate_temp_up", "nudge faster"),
    [keys.h1]: b("hotcue_1_activate", "hot cue A"),
    [keys.h2]: b("hotcue_2_activate", "hot cue B"),
    [keys.h3]: b("hotcue_3_activate", "hot cue C"),
    [keys.h4]: b("hotcue_4_activate", "hot cue D"),
  };
}

export const KEYMAP: Record<string, KeyBinding> = {
  ...deckKeys(A, {
    cue: "KeyA", play: "KeyS", sync: "KeyD", pfl: "KeyF", jumpBack: "KeyQ", jumpFwd: "KeyW",
    loop: "KeyE", reloop: "KeyR", slower: "KeyZ", faster: "KeyX", h1: "Digit1", h2: "Digit2", h3: "Digit3", h4: "Digit4",
  }),
  ...deckKeys(B, {
    cue: "KeyJ", play: "KeyK", sync: "KeyL", pfl: "Semicolon", jumpBack: "KeyU", jumpFwd: "KeyI",
    loop: "KeyO", reloop: "KeyP", slower: "KeyM", faster: "Comma", h1: "Digit7", h2: "Digit8", h3: "Digit9", h4: "Digit0",
  }),
  ArrowUp: { group: "[Library]", key: "MoveVertical", kind: "value", value: -1, label: "previous track" },
  ArrowDown: { group: "[Library]", key: "MoveVertical", kind: "value", value: 1, label: "next track" },
  Tab: { group: "[Library]", key: "TogglePane", kind: "value", value: 1, label: "switch library / suggestions" },
  "Shift+ArrowLeft": { group: A, key: "LoadSelectedTrack", kind: "value", value: 1, label: "load selected to deck A" },
  "Shift+ArrowRight": { group: B, key: "LoadSelectedTrack", kind: "value", value: 1, label: "load selected to deck B" },
};

function typingIn(el: Element | null) {
  if (!el) return false;
  if (el instanceof HTMLInputElement) return !["button", "checkbox", "radio", "range"].includes(el.type);
  return el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement || (el as HTMLElement).isContentEditable;
}

export function installKeyboard(opts: {
  send: (b: KeyBinding, value: number) => void;
  isOn: (group: string, key: string) => boolean;
  onLayer: (typing: boolean) => void;
  focusSearch: () => void;
}) {
  const update = () => opts.onLayer(typingIn(document.activeElement));
  document.addEventListener("focusin", update);
  document.addEventListener("focusout", () => setTimeout(update));

  const lookup = (e: KeyboardEvent) => KEYMAP[`${e.shiftKey ? "Shift+" : ""}${e.code}`] ?? KEYMAP[e.code];

  document.addEventListener("keydown", (e) => {
    if (e.code === "Escape") {
      (document.activeElement as HTMLElement | null)?.blur();
      update();
      return;
    }
    if (typingIn(document.activeElement) || e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.code === "Slash") {
      e.preventDefault();
      opts.focusSearch();
      return;
    }
    // Shift + hot cue key clears it.
    const hot = KEYMAP[e.code];
    if (e.shiftKey && hot?.key.startsWith("hotcue_")) {
      e.preventDefault();
      if (!e.repeat) opts.send({ ...hot, key: hot.key.replace("_activate", "_clear") }, 1);
      return;
    }
    const b = lookup(e);
    if (!b) return;
    e.preventDefault();
    if (e.repeat && b.kind !== "value") return;
    if (b.kind === "toggle") opts.send(b, opts.isOn(b.group, b.key) ? 0 : 1);
    else if (b.kind === "value") opts.send(b, b.value ?? 1);
    else opts.send(b, 1);
  });

  document.addEventListener("keyup", (e) => {
    if (typingIn(document.activeElement)) return;
    const b = KEYMAP[e.code];
    if (b?.kind === "button") opts.send(b, 0);
  });
}

export function keymapHelp(): string {
  const name = (code: string) =>
    code.replace("Key", "").replace("Digit", "").replace("Semicolon", ";").replace("Comma", ",").replace("Arrow", "").replace("Shift+", "Shift ");
  const rows = (g: string) =>
    Object.entries(KEYMAP)
      .filter(([, b]) => b.group === g && !b.key.startsWith("Load"))
      .map(([c, b]) => `<tr><td><kbd>${name(c)}</kbd></td><td>${b.label}</td></tr>`)
      .join("");
  const other = Object.entries(KEYMAP)
    .filter(([, b]) => b.group === "[Library]" || b.key === "LoadSelectedTrack")
    .map(([c, b]) => `<tr><td><kbd>${name(c)}</kbd></td><td>${b.label}</td></tr>`)
    .join("");
  return `<h3>Keyboard</h3>
    <p class="muted">Deck keys work unless a text field has focus. <kbd>Esc</kbd> returns to deck keys, <kbd>/</kbd> jumps to search. Shift + a hot cue key clears it.</p>
    <div style="display:flex;gap:28px">
      <table><tr><td colspan=2><b style="color:var(--deck-a)">Deck A</b></td></tr>${rows(A)}</table>
      <table><tr><td colspan=2><b style="color:var(--deck-b)">Deck B</b></td></tr>${rows(B)}</table>
      <table><tr><td colspan=2><b>Library</b></td></tr>${other}</table>
    </div>
    <div class="row"><button onclick="this.closest('dialog').close()">Close</button></div>`;
}
