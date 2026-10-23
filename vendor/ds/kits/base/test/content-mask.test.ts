import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createRawSnippet, flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import { KIT_ROOT } from "../tools/paths";
import {
  CONTENT_MASK_CLIP_PATHS,
  CONTENT_MASK_SHAPES,
  ContentMask,
  contentMask as contentMaskAppearance,
} from "./fixtures/content-mask-consumer";
import ContentMaskConsumer from "./fixtures/ContentMaskConsumer.svelte";
import { classes, classesOf, render, rendered, styleOf } from "./mount";

const REPO_ROOT = join(KIT_ROOT, "../..");
const image = createRawSnippet(() => ({
  render: () => '<img src="/ada.png" alt="Ada Lovelace">',
}));

describe("the Base ContentMask", () => {
  it("delivers every named daisyUI Mask shape through one structural contract", () => {
    expect(CONTENT_MASK_SHAPES).toEqual([
      "squircle",
      "heart",
      "hexagon",
      "hexagon-2",
      "decagon",
      "pentagon",
      "diamond",
      "square",
      "circle",
      "star",
      "star-2",
      "triangle",
      "triangle-2",
      "triangle-3",
      "triangle-4",
      "half-1",
      "half-2",
    ]);

    const clips = new Set<string>();
    for (const shape of CONTENT_MASK_SHAPES) {
      const mask = rendered(render(ContentMask, { shape, children: image }));
      expect(mask.dataset.shape).toBe(shape);
      expect(styleOf(mask, "clip-path")).toBe(CONTENT_MASK_CLIP_PATHS[shape]);
      clips.add(styleOf(mask, "clip-path"));
    }
    expect(clips.size).toBe(CONTENT_MASK_SHAPES.length);
  });

  it("clips presentation without replacing the content's accessible name", () => {
    const mask = rendered(render(ContentMask, {
      shape: "circle",
      children: image,
      title: "Profile portrait",
      class: "size-32",
      style: "margin: 1px",
    }));
    const content = mask.querySelector<HTMLImageElement>("img")!;

    expect(content.alt).toBe("Ada Lovelace");
    expect(content.closest("[aria-hidden=true]")).toBeNull();
    expect(mask.getAttribute("role")).toBeNull();
    expect(mask.title).toBe("Profile portrait");
    expect(styleOf(mask, "margin")).toBe("1px");
    expect(classes(mask)).toEqual(
      classesOf(contentMaskAppearance({ class: "size-32" })),
    );
  });

  it("keeps a clipped canonical control keyboard reachable and operable", () => {
    const root = render(ContentMaskConsumer);
    const mask = root.querySelector<HTMLElement>("[data-masked-control]")!;
    const control = mask.querySelector<HTMLButtonElement>("button")!;

    expect(control.disabled).toBe(false);
    expect(control.tabIndex).toBe(0);
    control.focus();
    expect(document.activeElement).toBe(control);
    control.click();
    flushSync();
    expect(root.querySelector("[data-mask-activations]")?.textContent).toBe("1");
  });

  it("is Density-independent and inherits every nested appearance axis", () => {
    const scope = rendered(render(ContentMaskConsumer));
    const masks = scope.querySelectorAll<HTMLElement>("[data-content-mask]");

    expect(scope.matches(
      '[data-theme="marketing"][data-color-scheme="dark"][data-density="compact"][data-motion="reduced"]',
    )).toBe(true);
    expect(masks).toHaveLength(2);
    for (const mask of masks) {
      for (const axis of ["data-theme", "data-color-scheme", "data-density", "data-motion"]) {
        expect(mask.hasAttribute(axis)).toBe(false);
      }
      expect(mask.className).not.toContain("--reddb-spatial");
      expect(mask.className).not.toContain("--reddb-color");
    }
  });

  it("ships every shape in the showcase and contract documentation", () => {
    const showcase = readFileSync(
      join(REPO_ROOT, "apps/showcase/src/routes/kit/content-mask/+page.svelte"),
      "utf8",
    );
    const documentation = readFileSync(join(KIT_ROOT, "README.md"), "utf8");

    expect(showcase).toContain("CONTENT_MASK_SHAPES");
    expect(showcase).toContain("{#each CONTENT_MASK_SHAPES as shape");
    expect(showcase).toContain("<ContentMask {shape}");
    for (const shape of CONTENT_MASK_SHAPES) {
      expect(documentation).toContain(`\`${shape}\``);
    }
  });

  it("ships consumer compilation, distributed export, and readiness evidence", () => {
    const definitions = JSON.parse(
      readFileSync(join(REPO_ROOT, "scripts/producer/readiness.json"), "utf8"),
    ) as { id: string; export: string; canonicalKit: string }[];
    const baseline = JSON.parse(
      readFileSync(join(REPO_ROOT, "packages/baseline/catalogues/capabilities.json"), "utf8"),
    ) as { id: string; status: string }[];

    expect(ContentMask).toBeDefined();
    expect(render(ContentMaskConsumer).querySelector("[data-content-mask]")).not.toBeNull();
    expect(definitions.find(({ id }) => id === "content-mask")).toMatchObject({
      export: "ContentMask",
      canonicalKit: "base",
    });
    expect(baseline.find(({ id }) => id === "content-mask")?.status).toBe("implemented");
  });
});
