// Bridge to the Rust core. Outside Tauri (plain `npm run dev` in a browser) a small
// mock stands in, so the UI can be worked on and tested without the native app.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import { mockInvoke, mockListen } from "./mock";

export const inTauri = "__TAURI_INTERNALS__" in window;

export function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return inTauri ? tauriInvoke<T>(cmd, args) : mockInvoke<T>(cmd, args ?? {});
}

export function listen<T>(event: string, cb: (payload: T) => void): void {
  if (inTauri) void tauriListen<T>(event, (e) => cb(e.payload));
  else mockListen(event, cb as (p: unknown) => void);
}

/** Send a control-bus value: `[Channel1] play = 1`. */
export function control(group: string, key: string, value: number): void {
  invoke("control", { group, key, value }).catch((e) => console.warn(`${group} ${key}:`, e));
}
