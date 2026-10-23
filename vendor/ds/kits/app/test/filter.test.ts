import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import Filter from "../src/composites/Filter.svelte";
import { filter } from "../src/composites/filter.variants";
import FilterContractFailures from "./fixtures/FilterContractFailures.svelte";
import FilterSurfacesConsumer from "./fixtures/FilterSurfacesConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

const OPTIONS = [
  { value: "all", label: "All" },
  { value: "svelte", label: "Svelte" },
  { value: "react", label: "React" },
] as const;
const REPO_ROOT = join(import.meta.dirname, "..", "..", "..");

function activeStateFailures(root: HTMLElement): string[] {
  const controls = [...root.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")];
  return controls.filter((control) => control.ariaPressed === "true").length === 1
    ? []
    : ["filter does not announce exactly one active option"];
}

describe("the deliberately failing Filter fixture", () => {
  it("diagnoses a filter whose active state is only visual", () => {
    const broken = rendered(
      render(FilterContractFailures, { failure: "active-state-is-only-visual" }),
    );

    expect(activeStateFailures(broken)).toEqual([
      "filter does not announce exactly one active option",
    ]);
  });
});

describe("Filter", () => {
  it("is consumer-compiled with CategoryFilter through the distributed app subpath", () => {
    const scope = rendered(render(FilterSurfacesConsumer));

    expect(scope.matches('[data-theme="application"][data-color-scheme="dark"][data-density="compact"]'))
      .toBe(true);
    expect(scope.querySelector("[data-filter]")).not.toBeNull();
    expect(scope.querySelector("[data-category-filter]")).not.toBeNull();
  });

  it("composes one named, announced active filter from the canonical ToggleGroup", () => {
    const root = rendered(
      render(Filter, {
        legend: "Framework",
        options: OPTIONS,
        value: "svelte",
        name: "framework",
      }),
    );
    const controls = [...root.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")];

    expect(root.tagName).toBe("FIELDSET");
    expect(root.querySelector(":scope > legend")?.textContent).toBe("Framework");
    expect(controls.map((control) => control.ariaPressed)).toEqual(["false", "true", "false"]);
    expect(controls.map((control) => control.tabIndex)).toEqual([-1, 0, -1]);
    expect(activeStateFailures(root)).toEqual([]);
    expect(root.querySelector<HTMLInputElement>('input[name="framework"]')?.value).toBe("svelte");
  });

  it("moves focus and the announced active state with canonical keyboard behavior", () => {
    const onvaluechange = vi.fn();
    const root = rendered(
      render(Filter, {
        legend: "Framework",
        options: OPTIONS,
        value: "svelte",
        onvaluechange,
      }),
    );
    const controls = [...root.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")];

    controls[1]!.focus();
    controls[1]!.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true }),
    );
    flushSync();

    expect(document.activeElement).toBe(controls[2]);
    expect(controls.map((control) => control.ariaPressed)).toEqual(["false", "false", "true"]);
    expect(onvaluechange).toHaveBeenCalledWith("react");
  });

  it("keeps Density tokenized and inherits every nested appearance axis", () => {
    const scope = document.createElement("div");
    scope.dataset.theme = "application";
    scope.dataset.colorScheme = "dark";
    scope.dataset.density = "compact";
    const root = render(Filter, { legend: "Framework", options: OPTIONS, value: "all" });
    scope.append(...root.children);
    const surface = scope.querySelector<HTMLElement>("[data-filter]")!;
    const list = surface.querySelector<HTMLElement>("[role='group']")!;
    const styles = filter();

    for (const name of classesOf(styles.root())) expect(classes(surface)).toContain(name);
    expect(classes(list)).toContain("gap-[var(--reddb-spatial-gap-sm)]");
    expect(surface.hasAttribute("data-theme")).toBe(false);
    expect(surface.hasAttribute("data-color-scheme")).toBe(false);
    expect(surface.hasAttribute("data-density")).toBe(false);
  });
});

describe("filter release readiness", () => {
  const capabilities = [
    ["filter", "Filter"],
    ["category-filter", "CategoryFilter"],
  ] as const;

  for (const [id, exported] of capabilities) {
    it(`${id} is implemented, showcased, consumer-compiled, and distributed`, () => {
      const definitions = JSON.parse(
        readFileSync(join(REPO_ROOT, "scripts", "producer", "readiness.json"), "utf8"),
      ) as Array<{
        id: string;
        baselineDisposition: string;
        canonicalKit: string;
        export: string;
        evidence: Record<string, string[]>;
      }>;
      const definition = definitions.find((candidate) => candidate.id === id);

      expect(definition).toMatchObject({
        id,
        baselineDisposition: "implemented",
        canonicalKit: "application",
        export: exported,
      });
      expect(Object.keys(definition!.evidence).sort()).toEqual([
        "accessibility",
        "appearance",
        "behavior",
        "consumerCompilation",
        "documentation",
        "showcase",
      ]);
      for (const paths of Object.values(definition!.evidence)) {
        expect(paths.length).toBeGreaterThan(0);
        for (const path of paths) expect(existsSync(join(REPO_ROOT, path)), path).toBe(true);
      }

      const snapshot = JSON.parse(
        readFileSync(
          join(REPO_ROOT, "packages", "baseline", "snapshot", "baseline-v1.json"),
          "utf8",
        ),
      ) as { capabilities: Array<{ id: string; classification: string; status: string }> };
      expect(snapshot.capabilities.find((capability) => capability.id === id)).toMatchObject({
        classification: "application",
        status: "implemented",
      });

      const barrel = readFileSync(join(REPO_ROOT, "kits", "app", "src", "index.ts"), "utf8");
      expect(barrel).toContain(`export { default as ${exported} }`);
    });
  }
});
