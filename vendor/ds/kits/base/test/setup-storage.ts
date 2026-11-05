// Node 26 ships its own `localStorage`/`sessionStorage` globals, which are
// `undefined` without `--localstorage-file` and shadow jsdom's Web Storage in
// Vitest. Point both back at the jsdom window so components that persist a
// preference (AppearanceSwitch) are tested against the browser API they use,
// on every Node the repo supports.
const dom = (globalThis as { jsdom?: { window: Record<string, unknown> } }).jsdom;
if (dom) {
  for (const key of ["localStorage", "sessionStorage"] as const) {
    Object.defineProperty(globalThis, key, {
      configurable: true,
      get: () => dom.window[key],
    });
  }
}
