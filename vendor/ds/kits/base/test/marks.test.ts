// The Marks this Kit ships are the pinned release's Marks — checked, not said.
//
// The Logo renders a drawing the Brand owns and the licence forbids altering
// (ADR 0004), and a Kit is distributed as source, so the Kit has to carry the
// Marks itself: a Logo whose drawing lived outside `src/` would arrive in a
// consumer with nothing to render. That copy is the risk this file exists for.
// `scripts/producer` already re-hashes `vendor/brand/marks/` against the pin;
// what nothing checked until now is that the Kit's copy is byte-for-byte the
// same, and that the release the Logo names in its refusal is the release the
// lock records.
//
// One pinned release, two places it has to hold — never two pins.

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { basename, join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  BRAND_RELEASE,
  LOGO_LAYOUTS,
  LOGO_SURFACES,
  MARKS,
  MARK_CATALOGUE,
} from "../src/logo.marks";
import { MARKS_DIR, VENDOR_DIR, kitMarkFiles } from "../tools/paths";

interface BrandLock {
  version: string;
  marks: Record<string, string>;
}

const lock = JSON.parse(readFileSync(join(VENDOR_DIR, "brand.lock.json"), "utf8")) as BrandLock;

/** SHA-256 of raw bytes, lowercase hex — the form the release publishes. */
function sha256(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

describe("the Marks the Kit vendors", () => {
  const shipped = kitMarkFiles().map((file) => basename(file));

  it("are the ones the pinned release published, by digest", () => {
    expect(shipped.length).toBeGreaterThan(0);
    for (const file of shipped) {
      expect(lock.marks[file], `${file} is not in the pinned release`).toBeDefined();
      expect(sha256(join(MARKS_DIR, file)), `${file} was edited locally`).toBe(lock.marks[file]);
    }
  });

  it("are all of them — none went missing on the way into the Kit", () => {
    const drawings = Object.keys(lock.marks).filter((file) => file.endsWith(".svg"));
    expect(shipped.sort()).toEqual(drawings.sort());
  });

  it("are byte-identical to the vendored Brand release", () => {
    // The stricter form of the digest check, and the one that says what to do:
    // if these differ, the fix is to re-copy from vendor/, never to edit here.
    for (const file of shipped) {
      expect(readFileSync(join(MARKS_DIR, file))).toEqual(
        readFileSync(join(VENDOR_DIR, "marks", file)),
      );
    }
  });
});

describe("the release the Logo names", () => {
  it("is the one the lock pins", () => {
    // The refusal quotes this version at a developer. A stale literal would
    // send them to look for a Mark in a release nobody is on.
    expect(BRAND_RELEASE).toBe(lock.version);
  });
});

describe("the catalogue and the files on disk", () => {
  const shipped = new Set(kitMarkFiles().map((file) => basename(file)));

  it("catalogues the symbol and wordmark drawing families", () => {
    expect(LOGO_LAYOUTS).toContain("symbol");
    expect(LOGO_LAYOUTS).toContain("wordmark");
    expect(MARKS.symbol.light?.file).toBe("reddb-symbol-black.svg");
    expect(MARKS.symbol.dark?.file).toBe("reddb-symbol-white.svg");
    expect(MARKS.wordmark.light?.file).toBe("reddb-wordmark-black.svg");
    expect(MARKS.wordmark.dark?.file).toBe("reddb-wordmark-white.svg");
  });

  it("catalogues every shipped drawing, including non-surface symbol treatments", () => {
    expect(MARK_CATALOGUE.map((mark) => mark.file).sort()).toEqual([...shipped].sort());
    expect(MARK_CATALOGUE.map((mark) => mark.file)).toContain("reddb-symbol-color.svg");
    expect(MARK_CATALOGUE.map((mark) => mark.file)).toContain("reddb-symbol-knockout.svg");
  });

  it("names only Marks that are there", () => {
    for (const layout of LOGO_LAYOUTS) {
      for (const on of LOGO_SURFACES) {
        const mark = MARKS[layout][on];
        if (mark === undefined) continue;
        expect(shipped.has(mark.file), `${mark.file} is in the catalogue but not on disk`).toBe(
          true,
        );
      }
    }
  });

  it("places every surface-specific Mark and leaves the treatments in the catalogue", () => {
    // Color and knockout name symbol treatments, not surfaces. Keeping them
    // out of this selection map preserves `on` as local-background vocabulary
    // while the complete catalogue above still ships and records both files.
    const placed = new Set(
      LOGO_LAYOUTS.flatMap((layout) =>
        LOGO_SURFACES.map((on) => MARKS[layout][on]?.file).filter(
          (file): file is string => file !== undefined,
        ),
      ),
    );
    expect([...placed].sort()).toEqual(
      [...shipped]
        .filter(
          (file) =>
            file !== "reddb-symbol-color.svg" && file !== "reddb-symbol-knockout.svg",
        )
        .sort(),
    );
    expect(shipped.has("reddb-icon-inverse.svg")).toBe(false);
  });

  it("records each drawing's own dimensions, as its viewBox states them", () => {
    // The box arithmetic scales the drawing, so a wrong dimension here would
    // silently distort a Mark — the one thing the licence does not allow.
    for (const mark of MARK_CATALOGUE) {
      const svg = readFileSync(join(MARKS_DIR, mark.file), "utf8");
      const viewBox = /viewBox="0 0 ([\d.]+) ([\d.]+)"/.exec(svg);
      expect(viewBox, `${mark.file} has no readable viewBox`).not.toBeNull();
      expect(Number(viewBox![1])).toBeCloseTo(mark.width, 5);
      expect(Number(viewBox![2])).toBeCloseTo(mark.height, 5);
      // The symbol is a share of that height, never more than the whole.
      expect(mark.symbolHeight).toBeGreaterThan(0);
      expect(mark.symbolHeight).toBeLessThanOrEqual(mark.height);
    }
  });
});
