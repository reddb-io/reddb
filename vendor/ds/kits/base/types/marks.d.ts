// What an `.svg` import is, for this repo's own compiler — and deliberately
// NOT for a consumer's.
//
// The Logo reaches a Mark the way every bundler already understands: it
// imports the file and gets back the URL the bundler emitted for it. That
// keeps the drawing opaque bytes the whole way through, which is what ADR 0004
// asks — the Brand owns the Mark, the DS places it and never reads it.
//
// The declaration lives in `types/`, outside `src/`, so it is NOT vendored
// with the Kit. A wildcard ambient module can only be declared once in a
// program, and every Vite or SvelteKit consumer already declares this one
// through `vite/client`; shipping a second copy inside the Kit would break a
// consumer's type-check the day they vendored it, with a duplicate-identifier
// error in a file they did not write.

declare module "*.svg" {
  /** The URL the bundler emits for the file. */
  const src: string;
  export default src;
}
