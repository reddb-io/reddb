/**
 * The one test file in this Kit that runs outside a document.
 *
 * Every other file here mounts a component, so the suite's environment is
 * jsdom; this one runs a production build, and esbuild refuses to start under
 * jsdom's `TextEncoder` — correctly, since a bundler has no business inside a
 * fake browser. What is under test is the output of a build, not the behavior
 * of a component, so node is the honest environment for it.
 *
 * @vitest-environment node
 */

// What a bundler keeps, read off a real build.
//
// Issue #48's second acceptance criterion, and a promise ADR 0005 made when it
// took Bits UI as the Composites' behavior base: "per-primitive importable so
// tree-shaking keeps only what a page uses". A promise like that is untestable
// by inspection — every module here looks importable — so this file makes it a
// measurement instead: two entry files, each importing one component through
// a real package subpath, are put through Vite's real production build, and
// what is asserted is the text of the output.
//
// Two directions, because either alone would be easy to satisfy by accident:
//
//   the menu's bundle   must carry the inherited Base Composite, the Button it
//                       composes, and the one Bits primitive it uses — and
//                       nothing else from either Kit or Bits;
//   the Button's bundle must carry no behavior base at all, which is what
//                       keeps a marketing page that imports one control from
//                       paying for a menu system it never renders.
//
// The markers are class strings the components write, one per component, and
// each is checked — against both Kits' whole shipped source, not just the
// modules catalogued below — to be written by exactly the component it names
// before it is used, so a marker that stopped being distinctive fails here
// rather than quietly making an assertion vacuous. That check reads source
// text because these builds are unminified: a class literal reaches the output
// verbatim, which is what makes `bundle.includes(marker)` readable at all.
//
// This is also what `"sideEffects": false` in the Kit's package.json is for.
// A Svelte component compiles to module-level code a bundler must assume is
// side-effecting unless the package says otherwise; without that field every
// Primitive in the barrel survives into every bundle, which is the state this
// file was written against and the reason the field exists.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { beforeAll, describe, expect, it } from "vitest";
import { build, type Rolldown } from "vite";
import { KIT_ROOT, filesUnder, kitSourceFiles } from "../tools/paths";
import { componentOf, componentsWriting, coverage, emitters, kebab } from "./markers";

/** Long enough for two production builds on a cold cache. */
const BUILD_TIMEOUT = 180_000;

/**
 * Bundle one entry the way an application would, and hand back the code.
 *
 * `svelte` stays external because a consumer's own copy provides it — bundling
 * the framework would drown the measurement in it. Everything else is bundled,
 * which is the point: `bits-ui` and the Kit have to be inside the output for
 * the question "what survived?" to have an answer.
 */
async function bundle(entry: string): Promise<string> {
  const result = await build({
    root: KIT_ROOT,
    configFile: false,
    logLevel: "silent",
    plugins: [svelte()],
    resolve: {
      conditions: ["browser"],
      // This build deliberately bypasses vite.config.ts. Resolve the public
      // Base subpath to the same export target so the bundle measures the
      // canonical consumer import rather than a workspace-only relative path.
      alias: [
        { find: /^@reddb-io\/design-system\/base\/recipes$/, replacement: join(KIT_ROOT, "..", "base", "src", "recipes.ts") },
        { find: /^@reddb-io\/design-system\/base$/, replacement: join(KIT_ROOT, "..", "base", "src", "index.ts") },
      ],
    },
    build: {
      write: false,
      minify: false,
      lib: { entry, formats: ["es"], fileName: "bundle" },
      rolldownOptions: { external: [/^svelte(\/|$)/] },
    },
  });

  const outputs = (Array.isArray(result) ? result[0]! : result) as Rolldown.RolldownOutput;
  return outputs.output
    .filter((chunk): chunk is Rolldown.OutputChunk => chunk.type === "chunk")
    .map((chunk) => chunk.code)
    .join("\n");
}

/**
 * A class string only one component writes — the evidence that the component
 * itself is in a bundle, rather than something that merely looks like it.
 *
 * Hand-picked rather than derived, because "distinctive" is a judgement about
 * the vocabulary; kept honest by the first test below, which reads every
 * variants module and fails if a marker is written by more or fewer than one.
 */
const MARKERS: Readonly<Record<string, string>> = {
  WorkspaceTabs: "@container/workspace-tabs",
  ChangeReview: "@container/change-review",
  ErrorBoundary: "@container/error-boundary",
  ShortcutHelp: "@container/shortcut-help",
  SecretValue: "@container/secret-value",
  EntityJourney: "@container/entity-journey",
  EntityInspector: "flex min-w-0 flex-col overflow-hidden rounded-[var(--reddb-radius-lg)] border border-muted bg-elevation-raised-surface text-foreground",
  BottomNavigation: "m-0 flex list-none items-stretch gap-[var(--reddb-spatial-gap-sm)] p-0",
  ActionPanel: "flex w-full flex-wrap items-center justify-end",
  ActivityFeed: "m-0 w-full min-w-0 gap-[var(--reddb-spatial-gap-md)]",
  ApplicationShell: "flex min-h-dvh w-full flex-col bg-elevation-base-surface text-foreground",
  // Not the disabled pair: quietControl() (wave 6A) writes that one too.
  Button: "hover:bg-primary-active active:bg-primary-active aria-expanded:bg-primary-active",
  CategoryFilter: "data-category-filter",
  CategoryPreview: "group h-full min-w-0 overflow-hidden",
  ChatMessage: "flex flex-wrap items-baseline gap-[var(--reddb-spatial-gap-sm)]",
  CheckoutForm: "m-0 font-medium text-feedback-danger-foreground",
  CommandPalette: "isolate min-w-0 text-foreground",
  CardHeading: "min-w-0 items-start pb-0",
  ContextMenu: "data-[state=open]:outline-hidden",
  Diff: "m-0 overflow-x-auto py-[var(--reddb-spatial-inset-sm)] font-mono text-sm leading-relaxed",
  DropdownMenu: "min-w-48 overflow-hidden p-[var(--reddb-spatial-inset-sm)]",
  Filter: "data-filter",
  Incentive: "isolate w-full min-w-0",
  Kbd: "rounded-sm border border-muted bg-transparent font-mono",
  ListRow: "flex w-full items-center border-b border-muted text-start",
  MultiColumnLayout: "@container/multi-column-layout",
  Menubar: "data-[command-menubar]:items-center",
  NavItem: "border-transparent bg-transparent text-ink-muted",
  NodeBadge: "rounded-full border border-muted px-2.5 py-1 text-xs leading-tight",
  OrderHistory: "flex w-full flex-col min-w-0 gap-[var(--reddb-spatial-layout-gap-sm)]",
  OrderSummary: "flex flex-col gap-[var(--reddb-spatial-layout-gap-sm)]",
  Pill: "-me-1 inline-flex items-center justify-center rounded-full",
  Review: "grid w-full min-w-0 gap-[var(--reddb-spatial-gap-md)]",
  SidebarNavigation: "m-0 flex list-none flex-col gap-[var(--reddb-spatial-gap-sm)] p-0",
  SidebarPanel: "flex min-h-0 min-w-0 flex-col bg-elevation-sunken-surface text-foreground",
  SidebarRail: "flex h-full min-h-0 w-[calc(var(--reddb-spatial-control-height-md)+2*var(--reddb-spatial-inset-sm))] shrink-0 flex-col items-center",
  SettingsRow: "data-settings-row",
  ShoppingCart: "flex min-w-0 flex-wrap items-end gap-[var(--reddb-spatial-gap-md)]",
  PageHeading: "min-w-0 flex-[1_1_20rem]",
  ProductFeature: "@container/product-feature",
  ProductList: "flex min-w-0 items-baseline justify-between",
  ProductOverview: "grid min-w-0 items-start gap-[var(--reddb-spatial-layout-gap-md)]",
  ProductQuickview: "isolate grid min-w-0 gap-[var(--reddb-spatial-layout-gap-md)]",
  SidebarLayout: "@container/sidebar-layout",
  SpeedDial: "flex min-w-40 flex-col gap-[var(--reddb-spatial-gap-sm)]",
  SplitView: "w-1 cursor-col-resize",
  StoreNavigation: "isolate flex w-full flex-col bg-background text-foreground",
  TaskList: "@container/task-list",
  TaskCard: "data-task-card",
  TelemetryRow: "data-telemetry-row",
  PropertyPanel: "data-property-panel",
  PhaseSummary: "data-phase-summary",
  PlaybackControls: "data-playback-controls",
  Toolbar: "data-[command-toolbar]:items-center",
};

/**
 * What the canonical DropdownMenu composes, and so what its bundle must
 * contain.
 *
 * Both are static imports in `DropdownMenu.svelte`: Button is the trigger, and
 * Kbd draws the key caps for an item's `shortcut` in the `row` snippet. Kbd
 * being here is a positive claim, not a tolerated exception — the assertion
 * below requires every name in this list to be present, so a Kbd that stopped
 * being composed fails just as loudly as one that started leaking.
 */
const COMPOSED = ["Button", "Kbd"] as const;

/**
 * How a Bits primitive is recognized in a bundle.
 *
 * Bits builds its `data-*` attributes at runtime — `data-${component}-${part}`
 * — so the only literal each primitive leaves behind is the declaration that
 * names it. That declaration is therefore the marker, and it is the strongest
 * one available: it is present exactly when the primitive's own module is.
 */
function bitsMarker(component: string): string {
  return `component: "${component}"`;
}

/** The primitive a DropdownMenu is built from: Bits' shared menu machinery. */
const MENU = "menu";

/**
 * Bits primitives a menu has no business dragging in. Not the whole library —
 * the ones whose names are unambiguous enough to be evidence, which is what a
 * marker has to be.
 */
const OTHER_BITS_PRIMITIVES = [
  "accordion",
  "avatar",
  "calendar",
  "checkbox",
  "collapsible",
  "command",
  "date-field",
  "dialog",
  "link-preview",
  "menubar",
  "navigation-menu",
  "pagination",
  "pin-input",
  "popover",
  "radio-group",
  "scroll-area",
  "select",
  "slider",
  "tabs",
  "toggle-group",
  "toolbar",
  "tooltip",
] as const;

/** Bits UI's own shipped modules, for checking the markers above are real. */
function bitsSource(): string {
  const entry = createRequire(import.meta.url).resolve("bits-ui");
  return filesUnder(dirname(entry))
    .filter((file) => file.endsWith(".js") || file.endsWith(".svelte"))
    .map((file) => readFileSync(file, "utf8"))
    .join("\n");
}

const BASE_SRC = join(KIT_ROOT, "..", "base", "src");

/** The components this file measures: the Kit's own, plus the ones it inherits. */
const variantsModules = [
  // Operational components share one visual recipe; their native root
  // attributes identify the individual contract instead of a shared class.
  ...kitSourceFiles().filter((file) =>
    file.endsWith(".variants.ts") && !file.endsWith("operational.variants.ts")),
  ...["TaskCard", "TelemetryRow", "PropertyPanel", "PhaseSummary", "PlaybackControls", "EntityJourney"]
    .map(name => join(KIT_ROOT, "src", "composites", `${name}.svelte`)),
  join(KIT_ROOT, "src", "entity-journey.variants.ts"),
  join(BASE_SRC, "button.variants.ts"),
  join(BASE_SRC, "dropdown-menu.variants.ts"),
  join(BASE_SRC, "kbd.variants.ts"),
  join(BASE_SRC, "secret-value.variants.ts"),
];

/**
 * Every source file a measured bundle can draw from.
 *
 * Deliberately wider than the catalogue. The catalogue is what this file
 * measures; the surface is everything the bundler could put in front of a
 * marker, which is both Kits' whole shipped source — markup included, since a
 * `class` attribute writes a string into a bundle the same way a variants
 * module does. A marker distinctive among the catalogued modules alone can
 * still be written by an uncatalogued Base component, and then a hit on it
 * attributes the bundle to a component that was never measured.
 */
const emittingSurface = [...kitSourceFiles(), ...kitSourceFiles(BASE_SRC)];

let menuBundle = "";
let buttonBundle = "";
let numberFieldBundle = "";
let taskListBundle = "";

beforeAll(async () => {
  menuBundle = await bundle(join("test", "fixtures", "tree-shaking", "menu-only.ts"));
  buttonBundle = await bundle(join("test", "fixtures", "tree-shaking", "button-only.ts"));
  numberFieldBundle = await bundle(join("test", "fixtures", "tree-shaking", "number-field-only.ts"));
  taskListBundle = await bundle(join("test", "fixtures", "tree-shaking", "task-list-only.ts"));
}, BUILD_TIMEOUT);

describe("independent lab workflow imports", () => {
  it("includes the selected contract and excludes unrelated workflows and heavy engines", () => {
    expect(numberFieldBundle).toContain("data-number-field");
    expect(numberFieldBundle).not.toContain("data-task-list");
    expect(taskListBundle).toContain("data-task-list");
    expect(taskListBundle).not.toContain("data-number-field");
    for (const code of [numberFieldBundle, taskListBundle]) {
      expect(code.length).toBeGreaterThan(1000);
      for (const name of [MENU, ...OTHER_BITS_PRIMITIVES]) expect(code).not.toContain(bitsMarker(name));
      for (const marker of ["data-terminal-session", "data-pan-zoom", "data-geo-map", "layerchart", "xterm", "graphology", "sigma"]) expect(code).not.toContain(marker);
    }
    expect(buttonBundle).not.toContain("data-number-field");
    expect(buttonBundle).not.toContain("data-task-list");
  });
});

describe("the markers this file measures with", () => {
  it("each name exactly one component's emitted output", () => {
    // A single name, not merely a single hit: a marker that turned out to be
    // some other component's idiom would still be "distinctive" by a count,
    // while attributing every bundle it appears in to the wrong component.
    const sources = emitters(emittingSurface);
    expect(sources.length).toBeGreaterThan(0);
    for (const [component, marker] of Object.entries(MARKERS)) {
      expect(
        componentsWriting(marker, sources),
        `${component}'s marker does not name ${component} alone`,
      ).toEqual([kebab(component)]);
    }
  });

  it("cover every component measured from the Application and inherited Base Kits", () => {
    // Otherwise an Application component added later would be absent from the
    // bundle assertions below, while the inherited Button must be measured at
    // the Base source that actually ships it. `markers.test.ts` holds the
    // comparison itself against a catalogue with a component missing from the
    // map, which is the state this assertion was in from #158 until #233.
    const gaps = coverage(variantsModules.map(componentOf), MARKERS);
    expect(gaps.unmarked, "catalogued components no marker measures").toEqual([]);
    expect(gaps.uncatalogued, "markers naming no catalogued component").toEqual([]);
  });

  it("name primitives Bits UI actually ships", () => {
    // An absence is only evidence if the thing could have been present: a Bits
    // primitive renamed upstream would make every `not.toContain` below pass
    // for the wrong reason, so the markers are checked against Bits' own source
    // rather than trusted.
    const source = bitsSource();
    for (const component of [MENU, ...OTHER_BITS_PRIMITIVES]) {
      expect(source, `bits-ui declares no "${component}"`).toContain(bitsMarker(component));
    }
  });
});

describe("an application that imports the inherited Base Composite", () => {
  it("gets a real bundle, with the Kit and its behavior base inside it", () => {
    // The bar every assertion below stands on: an empty or externalized
    // bundle would make every `not.toContain` pass for the wrong reason.
    expect(menuBundle.length).toBeGreaterThan(10_000);
    expect(menuBundle).toContain(MARKERS.DropdownMenu);
    expect(menuBundle).toContain(bitsMarker(MENU));
    // …and it is a dropdown menu, not some other menu: the variant is what
    // renames every `data-menu-*` attribute to `data-dropdown-menu-*`.
    expect(menuBundle).toContain('"dropdown-menu"');
  });

  it("gets the Primitives the Composite actually composes", () => {
    for (const component of COMPOSED) {
      expect(menuBundle, `${component} is composed but missing`).toContain(MARKERS[component]);
    }
  });

  it("gets none of the Primitives it does not", () => {
    const uninvited = Object.entries(MARKERS)
      .filter(([name]) => name !== "DropdownMenu" && !COMPOSED.includes(name as never))
      .filter(([, marker]) => menuBundle.includes(marker))
      .map(([name]) => name);
    expect(uninvited).toEqual([]);
  });

  it("gets none of Bits UI's other primitives", () => {
    // The per-import half of ADR 0005's claim: a page with a menu on it does
    // not ship a calendar, a dialog and a slider to get one.
    const uninvited = OTHER_BITS_PRIMITIVES.filter((component) =>
      menuBundle.includes(bitsMarker(component)),
    );
    expect(uninvited).toEqual([]);
  });
});

describe("an application that imports one Primitive", () => {
  it("gets that Primitive", () => {
    expect(buttonBundle.length).toBeGreaterThan(1_000);
    expect(buttonBundle).toContain(MARKERS.Button);
  });

  it("pays for no behavior base at all", () => {
    // A marketing surface importing one control should stay zero-JS beyond the
    // control (ADR 0005's first consequence). Bits hydrates only where it is
    // imported, and this is where that stops being a hope.
    for (const component of [MENU, ...OTHER_BITS_PRIMITIVES]) {
      expect(
        buttonBundle,
        `Bits' ${component} reached a bundle that imported no Composite`,
      ).not.toContain(bitsMarker(component));
    }
    expect(buttonBundle).not.toContain(MARKERS.DropdownMenu);
  });

  it("is smaller than the one that took the Composite", () => {
    // Not a threshold — a byte count would be a number to chase rather than a
    // property to keep. What is asserted is the direction: importing a
    // behavioral Composite costs something, and importing a Primitive does not
    // pay it.
    expect(buttonBundle.length).toBeLessThan(menuBundle.length);
  });
});
