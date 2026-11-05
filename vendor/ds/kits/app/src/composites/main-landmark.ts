// Who owns `<main>` (wave 5A, DESIGN.md "Landmarks").
//
// A page has exactly one top-level `main` landmark. ApplicationShell owns it:
// it is the page frame, and its skip link targets it. Every App layout that
// could also render `<main>` (SidebarLayout, a nested ApplicationShell) reads
// this context and renders a plain region instead when a layout above it has
// already rendered the landmark, so composing them never yields a second or a
// nested `main`. An `embedded` shell renders no `<main>` and claims nothing:
// its host places the landmark, possibly further down.

import { getContext, hasContext, setContext } from "svelte";

const MAIN_OWNED = Symbol("reddb.main-owned");

/** Declare that this subtree is inside a rendered `<main>` landmark. */
export function claimMainLandmark(): void {
  setContext(MAIN_OWNED, true);
}

/** Whether an enclosing layout already renders the page's `<main>`. */
export function mainLandmarkOwned(): boolean {
  return hasContext(MAIN_OWNED) && getContext<boolean>(MAIN_OWNED) === true;
}
