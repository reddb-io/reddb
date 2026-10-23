// NodeBadge, as an application meets it.
//
// The invariant worth a test here is that colour is never the only carrier of
// the status. Four statuses are drawn from two Theme-declared tokens, so a
// dot alone cannot distinguish them for anyone — the word is always in the
// DOM, and hiding it is only ever a visual choice.

import { describe, expect, it } from "vitest";
import NodeBadge from "../src/primitives/NodeBadge.svelte";
import { NODE_STATUSES, nodeBadge } from "../src/primitives/node-badge.variants";
import { classes, classesOf, render, rendered } from "./mount";

/** The dot, the name and the status word, in document order. */
function parts(element: HTMLElement): HTMLElement[] {
  return [...element.children] as HTMLElement[];
}

describe("NodeBadge", () => {
  it("renders the node's name", () => {
    const element = rendered(render(NodeBadge, { name: "reddb-01" }));
    expect(element.tagName).toBe("SPAN");
    const [, name] = parts(element);
    expect(name!.textContent).toBe("reddb-01");
    expect(classes(name!)).toEqual(classesOf(nodeBadge({}).name()));
  });

  it("names the status in text for every status it has", () => {
    for (const status of NODE_STATUSES) {
      const element = rendered(render(NodeBadge, { name: "reddb-01", status }));
      expect(element.textContent).toContain(status);
      expect(element.getAttribute("data-node-status")).toBe(status);
    }
  });

  it("keeps the status word in the DOM when it is not drawn", () => {
    const element = rendered(
      render(NodeBadge, { name: "reddb-01", status: "offline", showStatus: false }),
    );
    const word = parts(element).at(-1)!;
    expect(word.textContent).toBe("offline");
    expect(classes(word).has("sr-only")).toBe(true);
  });

  it("gives each status its own dot, and hides the dot from assistive tech", () => {
    const seen = new Set<string>();
    for (const status of NODE_STATUSES) {
      const element = rendered(render(NodeBadge, { name: "reddb-01", status }));
      const [dot] = parts(element);
      expect(dot!.getAttribute("aria-hidden")).toBe("true");
      expect(classes(dot!)).toEqual(classesOf(nodeBadge({ status }).dot()));
      seen.add(nodeBadge({ status }).dot());
    }
    // Two tokens, four statuses: fill is the second axis, so no two dots are
    // drawn the same way.
    expect(seen.size).toBe(NODE_STATUSES.length);
  });

  it("defaults to unknown, because no reading is not a good reading", () => {
    const element = rendered(render(NodeBadge, { name: "reddb-01" }));
    expect(element.getAttribute("data-node-status")).toBe("unknown");
    expect(classes(parts(element)[0]!)).toEqual(classesOf(nodeBadge({ status: "unknown" }).dot()));
  });

  it("wears exactly the classes its variants module produces", () => {
    for (const status of NODE_STATUSES) {
      const element = rendered(render(NodeBadge, { name: "reddb-01", status }));
      expect(classes(element)).toEqual(classesOf(nodeBadge({ status }).root()));
    }
  });

  it("merges a caller's classes over the root slot's own", () => {
    const element = rendered(render(NodeBadge, { name: "reddb-01", class: "max-w-40" }));
    expect(classes(element).has("max-w-40")).toBe(true);
    expect(classes(element).has("rounded-full")).toBe(true);
  });

  it("passes native attributes straight through", () => {
    const element = rendered(render(NodeBadge, { name: "reddb-01", id: "node-1" }));
    expect(element.id).toBe("node-1");
  });
});
