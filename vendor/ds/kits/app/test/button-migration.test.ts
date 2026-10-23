// The contracted Application-to-Base Button boundary.
//
// Active call sites name the canonical Base contract explicitly, Application
// owns no Button source, export or catalogue entry, and its route still exists
// only through the declared Base inheritance edge. The same guard holds the
// release evidence to that one canonical source.

import { existsSync, readFileSync, readdirSync } from "node:fs";
import { extname, join, relative } from "node:path";
import { describe, expect, it } from "vitest";
import * as Base from "@reddb-io/design-system/base";
import * as Application from "../src/index";

const REPO_ROOT = join(import.meta.dirname, "..", "..", "..");
const THIS_FILE = "kits/app/test/button-migration.test.ts";
const BUTTON_BINDINGS = new Set([
  "Button",
  "button",
  "buttonSpinner",
  "BUTTON_SIZES",
  "BUTTON_VARIANTS",
  "ButtonSize",
  "ButtonVariant",
  "ButtonVariants",
]);

function filesUnder(directory: string): string[] {
  return readdirSync(directory, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => join(entry.parentPath, entry.name));
}

const census = [
  join(REPO_ROOT, "README.md"),
  join(REPO_ROOT, "kits", "app", "README.md"),
  join(REPO_ROOT, "scripts", "producer", "README.md"),
  ...filesUnder(join(REPO_ROOT, "kits", "app", "src", "composites")),
  ...filesUnder(join(REPO_ROOT, "kits", "app", "test")),
  ...filesUnder(join(REPO_ROOT, "apps", "showcase", "src")),
].filter((file) => {
  const path = relative(REPO_ROOT, file);
  return path !== THIS_FILE && [".md", ".svelte", ".ts"].includes(extname(file));
});

/** Imported names in one static import declaration. */
function importedNames(declaration: string): string[] {
  const named = /\{([\s\S]*?)\}/.exec(declaration)?.[1] ?? "";
  return named
    .split(",")
    .map((binding) => binding.trim().replace(/^type\s+/, "").split(/\s+as\s+/)[0] ?? "")
    .filter(Boolean);
}

function legacyButtonBindings(file: string): string[] {
  const source = readFileSync(file, "utf8");
  const path = relative(REPO_ROOT, file);
  const violations: string[] = [];

  for (const match of source.matchAll(/^\s*import\s+[\s\S]*?\sfrom\s+["']([^"']+)["'];/gm)) {
    const declaration = match[0];
    const specifier = match[1]!;
    const names = importedNames(declaration).filter((name) => BUTTON_BINDINGS.has(name));
    const defaultButton = /^import\s+Button\s+from\b/.test(declaration);
    const applicationBarrel = specifier === "@reddb-io/design-system/app";
    const applicationSource = /(?:^|\/)primitives\/(?:Button\.svelte|button\.variants)$/.test(
      specifier,
    );
    const applicationFixtureBarrel = specifier === "../../../src/index";

    if (
      (applicationBarrel && names.length > 0) ||
      applicationSource ||
      (applicationFixtureBarrel && names.includes("Button")) ||
      (defaultButton && applicationSource)
    ) {
      violations.push(`${path}: ${names.join(", ") || "Button"} from ${specifier}`);
    }
  }

  return violations;
}

describe("the Application Button migration", () => {
  it("binds every active call site explicitly to the canonical Base contract", () => {
    expect(census.flatMap(legacyButtonBindings)).toEqual([]);
  });

  it("contracts every canonical surface to the inherited Base Button", () => {
    const legacySources = [
      join(REPO_ROOT, "kits", "app", "src", "primitives", "Button.svelte"),
      join(REPO_ROOT, "kits", "app", "src", "primitives", "button.variants.ts"),
    ];
    expect(legacySources.filter(existsSync)).toEqual([]);

    expect(Object.keys(Application).filter((name) => BUTTON_BINDINGS.has(name))).toEqual([]);
    expect(Application.PRIMITIVES).not.toContain("Button");
    expect(Base.BASE_COMPONENTS).toContain("Button");

    const routeOwners = [
      ...Application.PRIMITIVES.map((name) => ({ name, kit: "app" })),
      ...Base.BASE_COMPONENTS.map((name) => ({ name, kit: "base" })),
    ].filter(({ name }) => name === "Button");
    expect(routeOwners).toEqual([{ name: "Button", kit: "base" }]);

    const applicationManifest = JSON.parse(
      readFileSync(join(REPO_ROOT, "kits", "app", "kit.json"), "utf8"),
    ) as { parents?: string[] };
    expect(applicationManifest.parents).toContain("base");

    const capabilities = JSON.parse(
      readFileSync(
        join(REPO_ROOT, "packages", "baseline", "catalogues", "capabilities.json"),
        "utf8",
      ),
    ) as Array<{ id: string; classification: string; status: string }>;
    expect(capabilities.find(({ id }) => id === "button")).toEqual(
      expect.objectContaining({ classification: "base", status: "implemented" }),
    );

    const readiness = JSON.parse(
      readFileSync(join(REPO_ROOT, "scripts", "producer", "readiness.json"), "utf8"),
    ) as Array<{
      id: string;
      canonicalKit: string;
      export: string;
      evidence: Record<string, string[]>;
    }>;
    const button = readiness.find(({ id }) => id === "button");
    expect(button).toEqual(
      expect.objectContaining({ canonicalKit: "base", export: "Button" }),
    );
    expect(Object.values(button?.evidence ?? {}).every((paths) => paths.length > 0)).toBe(true);
  });
});
