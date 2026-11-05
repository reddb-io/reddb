// The injectable navigation seam (issue 483, maintainer decision 2026-10-05).
//
// Some consumers never let the browser follow a link: the same application
// runs routed and embedded with no URL of its own, so every destination goes
// through an injected router. A Design System that hard-wired navigation into
// each link component would break that mode — and 29 components render links.
//
// So the seam is one context and one interception point instead of a prop on
// every component: a NavigationScope sets the context and takes the activation
// of any in-app `<a href>` inside it, whichever component (or caller) rendered
// it. Without a scope nothing changes: links are native anchors, as today.

import { getContext, setContext } from "svelte";
import { createNavigationDisclosures } from "./core/navigation-disclosures";

import type { NavigationSeam } from "./core/navigation";
export { ownsSameOrigin, routableAnchor, type NavigationSeam } from "./core/navigation";

const NAVIGATION = Symbol("reddb.navigation");

/** Provide `seam` to the components below (NavigationScope does this). */
export function setNavigation(seam: NavigationSeam): NavigationSeam {
  return setContext(NAVIGATION, seam);
}

/** The seam in force here, or `undefined` outside any NavigationScope. */
export function getNavigation(): NavigationSeam | undefined {
  return getContext<NavigationSeam | undefined>(NAVIGATION);
}

/** A shell-scoped coordinator for mutually exclusive navigation disclosures. */
interface NavigationDisclosures {
  register(close: () => void): { open: () => void; destroy: () => void };
}
const DISCLOSURES = Symbol("reddb-navigation-disclosures");

/** Establish an optional navigation disclosure scope for a page shell. */
export function provideNavigationDisclosures(): void {
  setContext<NavigationDisclosures>(DISCLOSURES, createNavigationDisclosures());
}

/** Join the nearest shell's disclosure scope; standalone navigation stays independent. */
export function navigationDisclosure(close: () => void): { open: () => void; destroy: () => void } | undefined {
  return getContext<NavigationDisclosures | undefined>(DISCLOSURES)?.register(close);
}
