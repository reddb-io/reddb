// The type Primitives: Heading, Text and Eyebrow (ADR 0025; audit DIR-3,
// CMP-12, media-07).
//
// What is pinned is the contract the ADR gives them: they consume ONLY the
// Theme's type roles — one `text-<role>` utility carries size, line height,
// tracking and weight — they take no raw size, weight or tracking, Heading's
// outline level is independent of its look, and none of them selects an
// appearance axis (a Marketing island makes them larger by itself).

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createRawSnippet } from "svelte";
import { describe, expect, it, vi } from "vitest";
import {
  EYEBROW_ELEMENTS,
  Eyebrow,
  HEADING_LEVELS,
  HEADING_ROLES,
  Heading,
  TEXT_ELEMENTS,
  TEXT_INKS,
  TEXT_ROLES,
  Text,
  eyebrow,
  heading,
  text,
} from "@reddb-io/design-system/base";
import { SRC_DIR } from "../tools/paths";
import { classes, classesOf, render, rendered } from "./mount";

const words = (content: string) => createRawSnippet(() => ({ render: () => `<span>${content}</span>` }));

/** Classes that would set type outside a role: a size step, a weight, a tracking or a line height. */
const RAW_TYPE_RE = /^(?:text-(?:xs|sm|base|lg|\d*xl|display-(?:sm|md|lg)|\[.*\])|font-(?:\w+|\[.*\])|tracking-.+|leading-.+)$/;
const ROLE_UTILITY_RE = /^text-(display|title|heading|body|caption|eyebrow)$/;

function typeClasses(element: Element): { roles: string[]; raw: string[] } {
  const all = [...classes(element)];
  return {
    roles: all.filter((name) => ROLE_UTILITY_RE.test(name)),
    raw: all.filter((name) => RAW_TYPE_RE.test(name) && !ROLE_UTILITY_RE.test(name)),
  };
}

function expectNoAxis(element: Element) {
  for (const attribute of ["data-theme", "data-color-scheme", "data-density"]) {
    expect(element.hasAttribute(attribute), attribute).toBe(false);
  }
}

describe("the Base Heading", () => {
  it("offers the Theme's heading roles and every outline level", () => {
    expect([...HEADING_ROLES]).toEqual(["display", "title", "heading"]);
    expect([...HEADING_LEVELS]).toEqual([1, 2, 3, 4, 5, 6]);
  });

  it("keeps the outline level separate from the role it reads as", () => {
    for (const level of HEADING_LEVELS) {
      for (const role of HEADING_ROLES) {
        const element = rendered(render(Heading, { level, role, children: words("Ship it") }));
        expect(element.tagName).toBe(`H${level}`);
        expect(element.dataset.typeRole).toBe(role);
        expect(classes(element)).toEqual(classesOf(heading({ role })));
      }
    }
  });

  it("reads as a title at level 1 and a heading below it when no role is chosen", () => {
    expect(rendered(render(Heading, { level: 1, children: words("Page") })).dataset.typeRole).toBe("title");
    expect(rendered(render(Heading, { children: words("Section") })).tagName).toBe("H2");
    for (const level of [2, 3, 4, 5, 6] as const) {
      expect(rendered(render(Heading, { level, children: words("Section") })).dataset.typeRole).toBe("heading");
    }
  });

  it("sets type only through its role, in ink, without selecting an axis", () => {
    for (const role of HEADING_ROLES) {
      const element = rendered(render(Heading, { role, children: words("Title") }));
      expect(typeClasses(element)).toEqual({ roles: [`text-${role}`], raw: [] });
      expect(classes(element).has("text-foreground")).toBe(true);
      expectNoAxis(element);
    }
  });

  it("forwards native attributes, such as the id an aria-labelledby points at", () => {
    const element = rendered(render(Heading, { id: "pricing", children: words("Pricing") }));
    expect(element.id).toBe("pricing");
    expect(element.textContent).toBe("Pricing");
  });
});

describe("the Base Text", () => {
  it("offers the Theme's reading roles, two inks and four elements", () => {
    expect([...TEXT_ROLES]).toEqual(["body", "caption"]);
    expect([...TEXT_INKS]).toEqual(["default", "muted"]);
    expect([...TEXT_ELEMENTS]).toEqual(["p", "span", "div", "small"]);
  });

  it("renders body copy in ink by default, a caption in muted ink", () => {
    const body = rendered(render(Text, { children: words("Copy") }));
    expect(body.tagName).toBe("P");
    expect(classes(body)).toEqual(classesOf(text({ role: "body", ink: "default" })));
    expect(classes(body).has("text-foreground")).toBe(true);

    const caption = rendered(render(Text, { role: "caption", children: words("Fig. 1") }));
    expect(classes(caption)).toEqual(classesOf(text({ role: "caption", ink: "muted" })));
    // Secondary text is ink-muted, never the `muted` surface role (AGENTS.md).
    expect(classes(caption).has("text-ink-muted")).toBe(true);
    expect(classes(caption).has("text-muted")).toBe(false);
  });

  it("lets the ink and the element be chosen, and nothing about its type", () => {
    for (const as of TEXT_ELEMENTS) {
      for (const role of TEXT_ROLES) {
        for (const ink of TEXT_INKS) {
          const element = rendered(render(Text, { as, role, ink, children: words("Copy") }));
          expect(element.tagName).toBe(as.toUpperCase());
          expect(element.dataset.typeRole).toBe(role);
          expect(typeClasses(element)).toEqual({ roles: [`text-${role}`], raw: [] });
          expectNoAxis(element);
        }
      }
    }
  });

  it("keeps the deprecated tone prop as a working alias that warns once in development", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const elements = [1, 2].map(() =>
        rendered(render(Text, { tone: "muted", children: words("Copy") })),
      );
      for (const element of elements) {
        expect(classes(element)).toEqual(classesOf(text({ role: "body", ink: "muted" })));
      }
      // `ink` wins over the alias.
      const both = rendered(render(Text, { ink: "default", tone: "muted", children: words("Copy") }));
      expect(classes(both)).toEqual(classesOf(text({ role: "body", ink: "default" })));
      expect(warn).toHaveBeenCalledTimes(1);
      expect(String(warn.mock.calls[0]![0])).toMatch(/\[reddb Text\] tone="muted" is deprecated.*ink="muted"/);
    } finally {
      warn.mockRestore();
    }
  });
});

describe("the Base Eyebrow", () => {
  it("is an uppercase micro-label in muted ink, sized and tracked by the eyebrow role", () => {
    const element = rendered(render(Eyebrow, { children: words("New") }));
    expect(element.tagName).toBe("P");
    expect(classes(element)).toEqual(classesOf(eyebrow()));
    expect(classes(element).has("uppercase")).toBe(true);
    expect(classes(element).has("text-ink-muted")).toBe(true);
    // One tracking — the role's — instead of the three the Kits spelled by hand.
    expect(typeClasses(element)).toEqual({ roles: ["text-eyebrow"], raw: [] });
    expectNoAxis(element);
  });

  it("labels a heading and is never one", () => {
    expect([...EYEBROW_ELEMENTS]).toEqual(["p", "span", "div"]);
    for (const as of EYEBROW_ELEMENTS) {
      expect(rendered(render(Eyebrow, { as, children: words("New") })).tagName).toBe(as.toUpperCase());
    }
  });
});

describe("the type roles under the Kit's class merger", () => {
  it("keeps a role beside its colour, and lets a caller's role replace it rather than stack", () => {
    // tailwind-merge reads an unknown text utility as a colour; the roles are declared as sizes.
    expect(classesOf(heading({ role: "display" }))).toEqual(new Set(["text-foreground", "text-balance", "text-display"]));
    const overridden = classesOf(text({ role: "body", class: "text-caption" }));
    expect(overridden.has("text-caption")).toBe(true);
    expect(overridden.has("text-body")).toBe(false);
    expect(overridden.has("text-foreground")).toBe(true);
    expect(classesOf(eyebrow({ class: "text-primary-text" })).has("text-eyebrow")).toBe(true);
  });
});

describe("the type Primitives' source", () => {
  it("names no type value outside the roles and offers no size, weight or tracking prop", () => {
    for (const name of ["heading", "text", "eyebrow"]) {
      const variants = readFileSync(join(SRC_DIR, `${name}.variants.ts`), "utf8");
      const literals = [...variants.matchAll(/"([^"\n]*)"/g)].flatMap((match) => match[1]!.split(/\s+/));
      expect(literals.filter((token) => RAW_TYPE_RE.test(token) && !ROLE_UTILITY_RE.test(token)), name).toEqual([]);
    }
    for (const component of ["Heading", "Text", "Eyebrow"]) {
      const source = readFileSync(join(SRC_DIR, `${component}.svelte`), "utf8");
      const props = /interface Props[^{]*\{([\s\S]*?)\n {2}\}/.exec(source)![1]!;
      expect(props, component).not.toMatch(/^\s*(size|weight|tracking|leading|fontSize)\??:/m);
    }
  });
});
