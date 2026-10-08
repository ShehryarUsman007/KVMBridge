// Browser `KeyboardEvent.code` -> Linux evdev key code (the wire format).

const MAP: Record<string, number> = {
  Escape: 1, Digit1: 2, Digit2: 3, Digit3: 4, Digit4: 5, Digit5: 6, Digit6: 7, Digit7: 8, Digit8: 9, Digit9: 10, Digit0: 11,
  Minus: 12, Equal: 13, Backspace: 14, Tab: 15, KeyQ: 16, KeyW: 17, KeyE: 18, KeyR: 19, KeyT: 20, KeyY: 21, KeyU: 22,
  KeyI: 23, KeyO: 24, KeyP: 25, BracketLeft: 26, BracketRight: 27, Enter: 28, ControlLeft: 29, KeyA: 30, KeyS: 31,
  KeyD: 32, KeyF: 33, KeyG: 34, KeyH: 35, KeyJ: 36, KeyK: 37, KeyL: 38, Semicolon: 39, Quote: 40, Backquote: 41,
  ShiftLeft: 42, Backslash: 43, KeyZ: 44, KeyX: 45, KeyC: 46, KeyV: 47, KeyB: 48, KeyN: 49, KeyM: 50, Comma: 51,
  Period: 52, Slash: 53, ShiftRight: 54, NumpadMultiply: 55, AltLeft: 56, Space: 57, CapsLock: 58, F1: 59, F2: 60,
  F3: 61, F4: 62, F5: 63, F6: 64, F7: 65, F8: 66, F9: 67, F10: 68, NumLock: 69, ScrollLock: 70, Numpad7: 71,
  Numpad8: 72, Numpad9: 73, NumpadSubtract: 74, Numpad4: 75, Numpad5: 76, Numpad6: 77, NumpadAdd: 78, Numpad1: 79,
  Numpad2: 80, Numpad3: 81, Numpad0: 82, NumpadDecimal: 83, IntlBackslash: 86, F11: 87, F12: 88, NumpadEnter: 96,
  ControlRight: 97, NumpadDivide: 98, PrintScreen: 99, AltRight: 100, Home: 102, ArrowUp: 103, PageUp: 104,
  ArrowLeft: 105, ArrowRight: 106, End: 107, ArrowDown: 108, PageDown: 109, Insert: 110, Delete: 111, Pause: 119,
  MetaLeft: 125, MetaRight: 126, ContextMenu: 127, F13: 183, F14: 184, F15: 185, F16: 186, F17: 187, F18: 188,
  F19: 189, F20: 190, F21: 191, F22: 192, F23: 193, F24: 194,
};

const NAMES: Record<number, string> = {
  1: "Esc", 14: "Backspace", 15: "Tab", 28: "Enter", 29: "Ctrl", 42: "Shift", 54: "Shift", 56: "Alt", 57: "Space",
  58: "CapsLock", 69: "NumLock", 70: "ScrollLock", 97: "Ctrl", 99: "PrintScreen", 100: "AltGr", 102: "Home",
  103: "Up", 104: "PageUp", 105: "Left", 106: "Right", 107: "End", 108: "Down", 109: "PageDown", 110: "Insert",
  111: "Delete", 119: "Pause", 125: "Super", 126: "Super", 127: "Menu", 87: "F11", 88: "F12",
};

export function evdevFromCode(code: string): number | undefined {
  return MAP[code];
}

export function keyName(evdev: number): string {
  if (NAMES[evdev]) return NAMES[evdev];
  if (evdev >= 59 && evdev <= 68) return `F${evdev - 58}`;
  if (evdev >= 183 && evdev <= 194) return `F${evdev - 170}`;
  const entry = Object.entries(MAP).find(([, v]) => v === evdev);
  if (!entry) return `Key ${evdev}`;
  return entry[0].replace(/^Key/, "").replace(/^Digit/, "").replace(/^Numpad/, "Num ");
}

/** Modifiers first, then the rest. */
export function sortKeys(keys: number[]): number[] {
  const order = [29, 97, 56, 100, 42, 54, 125, 126];
  return [...keys].sort((a, b) => {
    const ia = order.indexOf(a);
    const ib = order.indexOf(b);
    if (ia >= 0 || ib >= 0) return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib);
    return a - b;
  });
}
