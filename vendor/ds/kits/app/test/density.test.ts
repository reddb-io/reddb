// The Kit under the Density axis.
//
// ADR 0003 makes Density the DS's third axis and names the price: "component
// size maps must route spatial values through density-aware tokens instead of
// raw utilities". A Tailwind step compiles to a fixed rem, so a Kit written in
// steps is frozen against a stop the same way a Kit written in hex would be
// frozen against a Theme. This file is the proof that the price was paid, and
// it asks the three questions that make the migration true rather than merely
// done:
//
//   1. Does each spatial position name a density role instead of a step?
//   2. Does that role still render, under `comfortable`, exactly the length the
//      step it replaced rendered at? (The axis was adopted, not applied — the
//      Kit's appearance must not have moved.)
//   3. Does it render something else under `tiny`, `compact`, and `spacious`? (Otherwise
//      the routing is decoration.)
//
// The lengths come from the Tokens Layer's own artifacts, resolved through
// direct Brand references or the compact midpoint calculation the browser
// follows, so nothing here is a second opinion about what a stop means — it is
// the stop's own output, read back.
//
// What is NOT routed is as deliberate as what is. The axis ships nine spatial
// roles, three steps each of control height, inset and gap; a Kit value that is
// not one of them has nothing to route through, and routing it anyway would
// either move what `comfortable` renders or invent a role the Tokens Layer does
// not ship. That is the same refusal the colour rule already makes, so a Kbd's
// cap height (which tracks the text it sits in, and which density is forbidden
// to touch) and a spinner's icon scale stay on Tailwind's own steps.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { describe, expect, it } from "vitest";
import {
  Button,
  Card,
  EmptyState,
  Kbd,
  button,
  card,
  emptyState,
  kbdChord,
} from "@reddb-io/design-system/base";
import ListRow from "../src/primitives/ListRow.svelte";
import NavItem from "../src/primitives/NavItem.svelte";
import NodeBadge from "../src/primitives/NodeBadge.svelte";
import Pill from "../src/primitives/Pill.svelte";
import SplitView from "../src/primitives/SplitView.svelte";
import ApplicationShell from "../src/composites/ApplicationShell.svelte";
import CommandPalette from "../src/composites/CommandPalette.svelte";
import MultiColumnLayout from "../src/composites/MultiColumnLayout.svelte";
import ContextMenu from "../src/composites/ContextMenu.svelte";
import Menubar from "../src/composites/Menubar.svelte";
import SidebarLayout from "../src/composites/SidebarLayout.svelte";
import SpeedDial from "../src/composites/SpeedDial.svelte";
import Toolbar from "../src/composites/Toolbar.svelte";
import { listRow } from "../src/primitives/list-row.variants";
import { navItem } from "../src/primitives/nav-item.variants";
import { nodeBadge } from "../src/primitives/node-badge.variants";
import { pill } from "../src/primitives/pill.variants";
import { applicationShell } from "../src/composites/application-shell.variants";
import { commandPalette } from "../src/composites/command-palette.variants";
import { multiColumnLayout } from "../src/composites/multi-column-layout.variants";
import { contextMenu } from "../src/composites/context-menu.variants";
import { menubar } from "../src/composites/menubar.variants";
import { sidebarLayout } from "../src/composites/sidebar-layout.variants";
import { speedDial } from "../src/composites/speed-dial.variants";
import { toolbar } from "../src/composites/toolbar.variants";
import { kitSourceFiles } from "../tools/paths";
import { classesOf, render } from "./mount";

const require_ = createRequire(import.meta.url);

/** A generated artifact, read through the Tokens Layer's own exports map. */
function artifact(specifier: string): string {
  return readFileSync(require_.resolve(specifier), "utf8");
}

// A stop's artifact ends with its coarse-pointer floor (ADR 0022), a rule
// nested in \`@media (pointer: coarse)\`. Read as flat text it would replace
// every control height with the 44px floor, so the fine pointer (the default
// these assertions describe) reads the stop without it, and the floor is read
// on its own.
const COARSE_BLOCK_RE = /@media\s*\(pointer:\s*coarse\)\s*\{(?:[^{}]*\{[^{}]*\})*[^{}]*\}/g;

/** Every `--reddb-*: value` declaration in a generated stylesheet, under a fine pointer. */
function declarations(css: string): Map<string, string> {
  const declared = new Map<string, string>();
  for (const match of css.replace(COARSE_BLOCK_RE, "").matchAll(/(--reddb-[a-z0-9_-]+)\s*:\s*([^;]+);/g)) {
    declared.set(match[1]!, match[2]!.trim());
  }
  return declared;
}

/** The declarations a coarse pointer adds on top of a stop. */
function coarseDeclarations(css: string): Map<string, string> {
  const declared = new Map<string, string>();
  for (const block of css.match(COARSE_BLOCK_RE) ?? []) {
    for (const match of block.matchAll(/(--reddb-[a-z0-9_-]+)\s*:\s*([^;]+);/g)) {
      declared.set(match[1]!, match[2]!.trim());
    }
  }
  return declared;
}

/** The four stops of the axis (ADR 0003), each read from its own artifact. */
const STOPS = ["tiny", "compact", "comfortable", "spacious"] as const;
type Stop = (typeof STOPS)[number];

const brandScale = declarations(artifact("@reddb-io/tokens/tokens.css"));
const stops = new Map<Stop, Map<string, string>>(
  STOPS.map((stop) => [stop, declarations(artifact(`@reddb-io/tokens/density-${stop}.css`))]),
);

/**
 * The length `role` renders at under `stop` — the stop's reassignment followed
 * through to the step of the Brand's spacing scale it lands on.
 */
function renders(role: string, stop: Stop): string {
  const declared = stops.get(stop)!.get(`--reddb-spatial-${role}`);
  expect(declared, `the ${stop} stop declares no --reddb-spatial-${role}`).toBeDefined();
  const reference = /^var\(\s*(--reddb-[a-z0-9_-]+)\s*\)$/.exec(declared!);
  if (reference) {
    const length = brandScale.get(reference[1]!);
    expect(length, `nothing declares ${reference[1]}`).toBeDefined();
    return length!;
  }
  const midpoint = /^calc\(\(var\(\s*(--reddb-[a-z0-9_-]+)\s*\) \+ var\(\s*(--reddb-[a-z0-9_-]+)\s*\)\) \/ 2\)$/.exec(
    declared!,
  );
  expect(midpoint, `${role} at ${stop} is neither a Brand step nor its midpoint`).not.toBeNull();
  const left = brandScale.get(midpoint![1]!);
  const right = brandScale.get(midpoint![2]!);
  expect(left, `nothing declares ${midpoint![1]}`).toBeDefined();
  expect(right, `nothing declares ${midpoint![2]}`).toBeDefined();
  const leftLength = /^(\d+(?:\.\d+)?)([a-z%]+)$/.exec(left!);
  const rightLength = /^(\d+(?:\.\d+)?)([a-z%]+)$/.exec(right!);
  expect(leftLength?.[2]).toBe(rightLength?.[2]);
  return `${(Number(leftLength![1]) + Number(rightLength![1])) / 2}${leftLength![2]}`;
}

/** One spatial position a Kit class routes through the axis. */
interface Routed {
  /** The Tailwind position the value sits in: `h`, `px`, `py`, `pb`, `gap`. */
  position: string;
  /** The density role that position now names. */
  role: string;
  /** The fixed step it replaced… */
  was: string;
  /** …and the length that step rendered at, which `comfortable` must keep. */
  length: string;
}

/** One class string the Kit produces, and what it routes. */
interface Specimen {
  name: string;
  classes: () => string;
  routed: readonly Routed[];
}

const HEIGHT_SM: Routed = { position: "h", role: "control-height-sm", was: "h-6", length: "1.5rem" };
const HEIGHT_MD: Routed = {
  position: "h",
  role: "control-height-md",
  was: "h-8",
  length: "2rem",
};
const HEIGHT_LG: Routed = {
  position: "h",
  role: "control-height-lg",
  was: "h-10",
  length: "2.5rem",
};

const inset = (position: string, step: "sm" | "md" | "lg"): Routed =>
  ({
    sm: { position, role: "inset-sm", was: `${position}-3`, length: "0.75rem" },
    md: { position, role: "inset-md", was: `${position}-4`, length: "1rem" },
    lg: { position, role: "inset-lg", was: `${position}-6`, length: "1.5rem" },
  })[step];

const GAP_SM: Routed = { position: "gap", role: "gap-sm", was: "gap-1", length: "0.25rem" };
const GAP_MD: Routed = { position: "gap", role: "gap-md", was: "gap-2", length: "0.5rem" };
const GAP_LG: Routed = { position: "gap", role: "gap-lg", was: "gap-3", length: "0.75rem" };
// The layout tier (ADR 0024): the space between a layout primitive's own blocks.
const LAYOUT_GAP_MD: Routed = { position: "gap", role: "layout-gap-md", was: "gap-6", length: "1.5rem" };

// Every spatial value in the Kit that the axis owns, component by component.
// A position missing from a row is a value the axis ships no role for — see the
// header note; the guard below is what stops a routed one quietly coming back.
const SPECIMENS: readonly Specimen[] = [
  {
    name: "ApplicationShell header container",
    classes: () => applicationShell().headerContainer(),
    routed: [inset("py", "sm")],
  },
  {
    name: "ApplicationShell main container",
    classes: () => applicationShell().mainContainer(),
    routed: [inset("py", "lg")],
  },
  {
    name: "CommandPalette content",
    classes: () => commandPalette().content(),
    routed: [inset("p", "md")],
  },
  {
    name: "MultiColumnLayout root",
    classes: () => multiColumnLayout().root(),
    routed: [LAYOUT_GAP_MD],
  },
  {
    name: "SidebarLayout root",
    classes: () => sidebarLayout().root(),
    routed: [LAYOUT_GAP_MD],
  },
  {
    name: "Button, size sm",
    classes: () => button({ size: "sm" }),
    routed: [{ ...HEIGHT_SM, position: "min-h" }, inset("px", "sm"), GAP_MD],
  },
  {
    name: "Button, size md",
    classes: () => button({ size: "md" }),
    routed: [{ ...HEIGHT_MD, position: "min-h" }, inset("px", "md"), GAP_MD],
  },
  {
    name: "Button, size lg",
    classes: () => button({ size: "lg" }),
    routed: [{ ...HEIGHT_LG, position: "min-h" }, inset("px", "lg"), GAP_MD],
  },
  {
    name: "Card header, padding md",
    classes: () => card({ padding: "md" }).header(),
    routed: [inset("p", "md"), GAP_SM],
  },
  { name: "Card body, padding md", classes: () => card({ padding: "md" }).body(), routed: [inset("p", "md")] },
  {
    name: "Card footer, padding md",
    classes: () => card({ padding: "md" }).footer(),
    routed: [inset("p", "md"), GAP_MD],
  },
  {
    name: "Card header, padding sm",
    classes: () => card({ padding: "sm" }).header(),
    routed: [inset("p", "sm"), GAP_SM],
  },
  { name: "Card body, padding sm", classes: () => card({ padding: "sm" }).body(), routed: [inset("p", "sm")] },
  {
    name: "Card footer, padding sm",
    classes: () => card({ padding: "sm" }).footer(),
    routed: [inset("p", "sm"), GAP_MD],
  },
  {
    name: "EmptyState root, size sm",
    classes: () => emptyState({ size: "sm" }).root(),
    routed: [GAP_MD, inset("px", "md"), inset("py", "lg")],
  },
  {
    name: "EmptyState root, size md",
    classes: () => emptyState({ size: "md" }).root(),
    routed: [GAP_LG, inset("px", "lg")],
  },
  { name: "EmptyState actions", classes: () => emptyState().actions(), routed: [GAP_MD] },
  { name: "Kbd chord", classes: () => kbdChord().root(), routed: [GAP_SM] },
  {
    name: "ListRow root, size md",
    classes: () => listRow({ size: "md" }).root(),
    routed: [GAP_LG, inset("px", "md"), inset("py", "sm")],
  },
  {
    name: "ListRow root, size sm",
    classes: () => listRow({ size: "sm" }).root(),
    routed: [GAP_MD, inset("px", "sm")],
  },
  { name: "ListRow trailing", classes: () => listRow().trailing(), routed: [GAP_MD] },
  { name: "NavItem root", classes: () => navItem().root(), routed: [GAP_MD, inset("px", "sm")] },
  { name: "NodeBadge root", classes: () => nodeBadge().root(), routed: [GAP_MD] },
  { name: "Pill, size md", classes: () => pill({ size: "md" }), routed: [inset("px", "sm")] },
  {
    name: "ContextMenu item, size md",
    classes: () => contextMenu({ size: "md" }).item(),
    routed: [HEIGHT_MD, inset("px", "md")],
  },
  {
    name: "ContextMenu content",
    classes: () => contextMenu().content(),
    routed: [inset("p", "sm")],
  },
  {
    name: "Menubar root",
    classes: () => menubar().root(),
    routed: [GAP_SM],
  },
  {
    name: "Menubar item, size md",
    classes: () => menubar({ size: "md" }).item(),
    routed: [HEIGHT_MD, inset("px", "md")],
  },
  {
    name: "Toolbar root",
    classes: () => toolbar().root(),
    routed: [GAP_SM, inset("p", "sm")],
  },
  {
    name: "Toolbar item, size md",
    classes: () => toolbar({ size: "md" }).item(),
    routed: [HEIGHT_MD, inset("px", "md")],
  },
  {
    name: "SpeedDial content",
    classes: () => speedDial().content(),
    routed: [GAP_SM, inset("p", "sm")],
  },
];

/** Every fixed step the axis owns, i.e. every step the Kit stopped writing. */
const OWNED: readonly string[] = [
  ...new Set(SPECIMENS.flatMap((specimen) => specimen.routed.map((routed) => routed.was))),
].sort();

describe("every spatial value the Density axis owns is named through it", () => {
  for (const specimen of SPECIMENS) {
    it(`${specimen.name} names roles, not steps`, () => {
      const worn = classesOf(specimen.classes());
      for (const { position, role, was } of specimen.routed) {
        const named = worn.has(`${position}-[var(--reddb-spatial-${role})]`);
        expect(named, `${specimen.name} does not name ${role} in its ${position} position`).toBe(
          true,
        );
        expect(worn.has(was), `${specimen.name} still wears the fixed ${was}`).toBe(false);
      }
    });
  }
});

/** Roles the Density axis never shrinks below the WCAG 2.2 target size (#519). */
const TARGET_FLOORED = new Set(["control-height-sm"]);

describe("what a coarse pointer resolves the control heights to (ADR 0022)", () => {
  /** A Brand step, or the midpoint of two, in rem. */
  const rem = (value: string): number => {
    const step = /^var\(\s*(--reddb-[a-z0-9_-]+)\s*\)$/.exec(value);
    if (step) return Number.parseFloat(brandScale.get(step[1]!)!);
    const mid = /^calc\(\(var\(\s*(--reddb-[a-z0-9_-]+)\s*\) \+ var\(\s*(--reddb-[a-z0-9_-]+)\s*\)\) \/ 2\)$/.exec(value);
    expect(mid, `${value} is neither a Brand step nor its midpoint`).not.toBeNull();
    return (Number.parseFloat(brandScale.get(mid![1]!)!) + Number.parseFloat(brandScale.get(mid![2]!)!)) / 2;
  };

  for (const stop of STOPS) {
    it(`raises every control height to a 44px target at ${stop}`, () => {
      const coarse = coarseDeclarations(artifact(`@reddb-io/tokens/density-${stop}.css`));
      for (const size of ["sm", "md", "lg"]) {
        const declared = coarse.get(`--reddb-spatial-control-height-${size}`);
        expect(declared, `${stop} declares no coarse control-height-${size}`).toBeDefined();
        expect(rem(declared!), `control-height-${size} at ${stop}`).toBeGreaterThanOrEqual(2.75);
      }
    });
  }
});

describe("what a stop resolves those roles to", () => {
  const routed = new Map<string, Routed>(
    SPECIMENS.flatMap((specimen) => specimen.routed).map((entry) => [entry.role, entry]),
  );

  for (const [role, { was, length }] of routed) {
    it(`renders ${role} at ${length} under comfortable — what ${was} rendered at`, () => {
      // The neutral is the whole reason this migration is safe to land: adopting
      // the axis changes nothing until a consumer declares another stop.
      expect(renders(role, "comfortable")).toBe(length);
    });

    it(`moves ${role} monotonically from tiny through spacious`, () => {
      if (TARGET_FLOORED.has(role)) {
        // A pointer target holds the WCAG 2.2 24px floor (#519): it may not
        // shrink below comfortable, but it still grows at spacious.
        expect(renders(role, "tiny")).toBe(renders(role, "comfortable"));
        expect(renders(role, "compact")).toBe(renders(role, "comfortable"));
        expect(renders(role, "spacious")).not.toBe(length);
        return;
      }
      expect(Number.parseFloat(renders(role, "tiny"))).toBeLessThan(
        Number.parseFloat(renders(role, "compact")),
      );
      expect(renders(role, "compact")).not.toBe(length);
      expect(Number.parseFloat(renders(role, "compact"))).toBeLessThan(
        Number.parseFloat(renders(role, "comfortable")),
      );
      expect(renders(role, "spacious")).not.toBe(length);
    });
  }
});

/** Class names the Kit writes, read the way the anti-hardcode lint reads them. */
function classesWritten(file: string): string[] {
  const source = readFileSync(file, "utf8");
  const pattern = file.endsWith(".variants.ts")
    ? /"([^"\\\n]*)"|'([^'\\\n]*)'/g
    : /\bclass\s*=\s*(?:"([^"]*)"|'([^']*)')/g;
  return [...source.matchAll(pattern)]
    .flatMap((match) => (match[1] ?? match[2] ?? "").split(/\s+/))
    .filter(Boolean);
}

describe("the Kit's source", () => {
  it("writes no fixed step the axis owns", () => {
    // Prose may still name a step — this reads class positions only, exactly as
    // the anti-hardcode lint does, so the guard cannot be tripped by a comment.
    const owned = new Set(OWNED);
    const offenders = kitSourceFiles().flatMap((file) =>
      classesWritten(file)
        .filter((written) => owned.has(written))
        .map((written) => `${file}: ${written}`),
    );
    expect(OWNED.length).toBeGreaterThan(0);
    expect(offenders).toEqual([]);
  });

  it("names only spatial roles the Tokens Layer declares", () => {
    // A typo in a role name is invisible in the browser — the variable simply
    // resolves to nothing and the property is dropped — so it is caught here.
    const declared = new Set(stops.get("comfortable")!.keys());
    const named = new Set<string>();
    for (const file of kitSourceFiles()) {
      for (const match of readFileSync(file, "utf8").matchAll(/--reddb-spatial-[a-z0-9-]+/g)) {
        named.add(match[0]);
      }
    }
    expect(named.size).toBeGreaterThan(0);
    expect([...named].filter((name) => !declared.has(name))).toEqual([]);
  });
});

/** Every class worn anywhere in a rendered component's tree. */
function wornAnywhere(target: HTMLElement): Set<string> {
  const worn = new Set<string>();
  for (const element of target.querySelectorAll("*")) {
    for (const name of classesOf(element.getAttribute("class") ?? "")) worn.add(name);
  }
  return worn;
}

describe("no component renders a fixed step the axis owns", () => {
  // The variants modules are where the classes are written, but it is the
  // components that wear them — and a component that reached around its own
  // variants module would pass every assertion above.
  const MOUNTED: readonly [string, () => HTMLElement][] = [
    ["ApplicationShell", () => render(ApplicationShell, {})],
    [
      "CommandPalette",
      () => render(CommandPalette, {
        triggerLabel: "Search commands",
        contentLabel: "Commands",
        label: "Command",
      }),
    ],
    ["Button", () => render(Button, {})],
    ["Button, size sm", () => render(Button, { size: "sm" })],
    ["Button, size lg", () => render(Button, { size: "lg", loading: true })],
    ["Card", () => render(Card, { title: "alpha", description: "a node" })],
    ["Card, padding sm", () => render(Card, { title: "alpha", padding: "sm" })],
    ["EmptyState", () => render(EmptyState, { title: "No nodes yet", hint: "reddb node add" })],
    ["Kbd", () => render(Kbd, { keys: ["Ctrl", "K"] })],
    ["ListRow", () => render(ListRow, { title: "alpha", description: "a node" })],
    ["ListRow, size sm", () => render(ListRow, { title: "alpha", size: "sm" })],
    ["NavItem", () => render(NavItem, { label: "Nodes" })],
    ["NodeBadge", () => render(NodeBadge, { name: "alpha" })],
    ["Pill", () => render(Pill, {})],
    ["SplitView", () => render(SplitView, {})],
    ["ContextMenu", () => render(ContextMenu, { triggerLabel: "Actions", contentLabel: "Actions" })],
    ["Menubar", () => render(Menubar, { label: "Commands" })],
    ["MultiColumnLayout", () => render(MultiColumnLayout, {})],
    ["SidebarLayout", () => render(SidebarLayout, {})],
    ["SpeedDial", () => render(SpeedDial, { triggerLabel: "Create", contentLabel: "Create actions" })],
    ["Toolbar", () => render(Toolbar, { label: "Formatting" })],
  ];

  const owned = new Set(OWNED);

  for (const [name, mount] of MOUNTED) {
    it(`${name} wears none of them`, () => {
      const worn = [...wornAnywhere(mount())].filter((written) => owned.has(written));
      expect(worn).toEqual([]);
    });
  }
});
