import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import * as Application from "../src/index";
import * as Base from "@reddb-io/design-system/base";

const REPO_ROOT = join(import.meta.dirname, "..", "..", "..");

interface Capability {
  id: string;
  classification: string;
  status: string;
}

interface Readiness {
  id: string;
  baselineDisposition: string;
  canonicalKit: string;
  export: string;
}

describe("the Application-to-Base contraction", () => {
  it("never exports a component that shadows an implemented Base capability", () => {
    const capabilities = JSON.parse(
      readFileSync(
        join(REPO_ROOT, "packages", "baseline", "catalogues", "capabilities.json"),
        "utf8",
      ),
    ) as Capability[];
    const readiness = JSON.parse(
      readFileSync(join(REPO_ROOT, "scripts", "producer", "readiness.json"), "utf8"),
    ) as Readiness[];

    const implementedBaseIds = new Set(
      capabilities
        .filter(({ classification, status }) => classification === "base" && status === "implemented")
        .map(({ id }) => id),
    );
    const implementedBaseExports = readiness
      .filter(
        ({ id, baselineDisposition, canonicalKit }) =>
          implementedBaseIds.has(id) &&
          baselineDisposition === "implemented" &&
          canonicalKit === "base",
      )
      .map(({ export: exported }) => exported);

    expect(implementedBaseExports).toEqual(
      expect.arrayContaining(["DropdownMenu", "Navbar"]),
    );
    expect(
      implementedBaseExports.filter((exported) => !(exported in Base)),
      "implemented Base capabilities must resolve from the Base subpath",
    ).toEqual([]);
    expect(
      implementedBaseExports.filter((exported) => exported in Application),
      "Application must inherit canonical Base contracts instead of shadowing them",
    ).toEqual([]);
  });
});
