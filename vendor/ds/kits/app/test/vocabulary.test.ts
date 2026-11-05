// One semantic vocabulary (ADR 0026), as the application Kit speaks it.
//
//   - Meaning is `tone`, imported from Base: a Pill's tone is the same
//     Feedback Role as a Badge's, pair for pair, and the accent is never a tone.
//   - A domain reading that is not a tone (NodeBadge's reachability) reaches the
//     vocabulary through a documented mapping, never through the Brand red.
//   - Size is `size`, and the current destination is `current` on an item and
//     `currentId` on a container. Density words never name a component prop.
//   - Every renamed public name stays one release as an alias that still works
//     and warns once per session in development.

import { badge, TONES } from "@reddb-io/design-system/base";
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import { quietControl } from "@reddb-io/design-system/base";
import {
  DEPRECATED_LIST_ROW_DENSITIES,
  DEPRECATED_PILL_VARIANTS,
  ListRow,
  listRow,
  NavItem,
  navItem,
  NODE_STATUS_TONES,
  NODE_STATUSES,
  NodeBadge,
  nodeBadge,
  Pill,
  pill,
  PILL_TONES,
  PILL_VARIANTS,
  SidebarRail,
} from "../src/index";
import { classes, classesOf, render, rendered, text } from "./mount";

let warn: MockInstance<typeof console.warn>;
beforeEach(() => {
  warn = vi.spyOn(console, "warn").mockImplementation(() => {});
});
afterEach(() => {
  warn.mockRestore();
});

const warnings = () =>
  warn.mock.calls.map(([message]) => String(message)).filter((message) => message.startsWith("[reddb "));

/** The colour classes of a recipe's output: borders, grounds and text colours. */
function colours(className: string): Set<string> {
  return new Set([...classesOf(className)].filter((name) => /^(?:border|bg|text)-(?!xs$|sm$|base$)/.test(name)));
}

describe("Pill's tone", () => {
  it("is the shared vocabulary, imported rather than restated", () => {
    expect(PILL_TONES).toBe(TONES);
  });

  it("names the same Feedback Role as Badge, for every tone and emphasis", () => {
    for (const variant of PILL_VARIANTS) {
      for (const tone of PILL_TONES) {
        expect(colours(pill({ tone, variant })), `${tone} ${variant}`).toEqual(colours(badge({ tone, variant })));
      }
    }
  });

  it("never fills a tone with the Brand red", () => {
    for (const variant of PILL_VARIANTS) {
      for (const tone of PILL_TONES) {
        expect(classesOf(pill({ tone, variant })).has("bg-primary"), `${tone} ${variant}`).toBe(false);
      }
    }
  });

  it("announces its tone and emphasis on the element", () => {
    const element = rendered(render(Pill, { tone: "warning", variant: "outline", children: text("lagging") }));
    expect(element.dataset.tone).toBe("warning");
    expect(element.dataset.variant).toBe("outline");
    expect(classes(element)).toEqual(classesOf(pill({ tone: "warning", variant: "outline" })));
  });
});

describe("NodeBadge's reachability", () => {
  it("maps every reading onto a tone, documented and exported", () => {
    expect(Object.keys(NODE_STATUS_TONES).sort()).toEqual([...NODE_STATUSES].sort());
    expect(NODE_STATUS_TONES).toEqual({
      online: "success",
      degraded: "warning",
      offline: "danger",
      unknown: "neutral",
    });
    for (const tone of Object.values(NODE_STATUS_TONES)) expect(TONES).toContain(tone);
  });

  it("draws each reading in its tone's Feedback Role, and never in the Brand red", () => {
    for (const status of NODE_STATUSES) {
      const tone = NODE_STATUS_TONES[status];
      const dot = classesOf(nodeBadge({ status }).dot());
      expect([...dot].some((name) => /(?:^|-)primary\b/.test(name)), status).toBe(false);
      if (tone !== "neutral") {
        expect(dot.has(`bg-[var(--reddb-color-feedback-${tone}-foreground)]`), status).toBe(true);
      }
      const element = rendered(render(NodeBadge, { name: "reddb-01", status }));
      expect(element.dataset.tone).toBe(tone);
      expect(element.dataset.nodeStatus).toBe(status);
    }
  });
});

describe("the size scale and the current destination", () => {
  it("sizes ListRow with size, never a Density word", () => {
    const element = rendered(render(ListRow, { title: "reddb-01", size: "sm" }));
    expect(element.dataset.size).toBe("sm");
    expect(classes(element)).toEqual(classesOf(listRow({ size: "sm" }).root()));
  });

  it("marks NavItem's current destination with current", () => {
    const element = rendered(render(NavItem, { label: "Tokens", href: "/tokens", current: true }));
    expect(element.getAttribute("aria-current")).toBe("page");
    expect(classes(element)).toEqual(classesOf(quietControl({ selected: true, class: navItem({ current: true }).root() })));
  });

  it("marks SidebarRail's current item with currentId", () => {
    const root = render(SidebarRail, {
      label: "Organizations",
      items: [
        { id: "reddb", label: "RedDB" },
        { id: "orbit", label: "Orbit" },
      ],
      currentId: "orbit",
    });
    expect(root.querySelector('[data-sidebar-rail-item="orbit"]')!.getAttribute("aria-pressed")).toBe("true");
    expect(root.querySelector('[data-sidebar-rail-item="reddb"]')!.getAttribute("aria-pressed")).toBe("false");
  });
});

describe("the deprecated aliases (one release)", () => {
  it("keep Pill's neutral and primary variants drawing as before, warning once each", () => {
    for (const _ of [1, 2]) {
      const neutral = rendered(render(Pill, { variant: "neutral" }));
      expect(colours(neutral.getAttribute("class")!)).toEqual(colours(pill({ variant: "tinted" })));
    }
    const primary = rendered(render(Pill, { variant: "primary" }));
    expect(classes(primary).has("bg-primary")).toBe(true);
    expect(DEPRECATED_PILL_VARIANTS).toEqual({ neutral: "tinted", primary: "filled" });
    expect(warnings()).toEqual([
      expect.stringMatching(/\[reddb Pill\] variant="neutral" is deprecated.*variant="tinted"/),
      expect.stringMatching(/\[reddb Pill\] variant="primary" is deprecated.*tone with variant="filled"/),
    ]);
  });

  it("keep ListRow's density working as the size it became, warning once", () => {
    for (const [density, size] of Object.entries(DEPRECATED_LIST_ROW_DENSITIES)) {
      for (const _ of [1, 2]) {
        const element = rendered(render(ListRow, { title: "reddb-01", density: density as "compact" }));
        expect(element.dataset.size).toBe(size);
        expect(classes(element)).toEqual(classesOf(listRow({ size }).root()));
      }
    }
    expect(warnings()).toEqual([
      expect.stringMatching(/\[reddb ListRow\] density="comfortable" is deprecated.*size="md"/),
      expect.stringMatching(/\[reddb ListRow\] density="compact" is deprecated.*size="sm"/),
    ]);
  });

  it("keep NavItem's active, with its old meaning for the token, warning once", () => {
    const active = rendered(render(NavItem, { label: "Details", href: "/2", active: true, current: "step" }));
    expect(active.getAttribute("aria-current")).toBe("step");
    // Under the alias a token alone does not make an item current, as before.
    const inactive = rendered(render(NavItem, { label: "Review", href: "/3", active: false, current: "step" }));
    expect(inactive.getAttribute("aria-current")).toBeNull();
    expect(warnings()).toEqual([expect.stringMatching(/\[reddb NavItem\] active is deprecated.*current/)]);
  });

  it("keep SidebarRail's selectedId, warning once", () => {
    const items = [
      { id: "reddb", label: "RedDB" },
      { id: "orbit", label: "Orbit" },
    ];
    for (const _ of [1, 2]) {
      const root = render(SidebarRail, { label: "Organizations", items, selectedId: "orbit" });
      expect(root.querySelector('[data-sidebar-rail-item="orbit"]')!.getAttribute("aria-pressed")).toBe("true");
    }
    expect(warnings()).toEqual([expect.stringMatching(/\[reddb SidebarRail\] selectedId is deprecated.*currentId/)]);
  });
});
