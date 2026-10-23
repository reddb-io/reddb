import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { assemblePackage } from "../../../scripts/producer/src/packaging";

const KIT_ROOT = join(import.meta.dirname, "..");

function json<T>(name: string): T {
  return JSON.parse(readFileSync(join(KIT_ROOT, name), "utf8")) as T;
}

describe("the Data Kit package contract", () => {
  it("is a general child of the Application Kit", () => {
    expect(json("kit.json")).toEqual({
      name: "data",
      audience: "*",
      parents: ["app"],
    });
  });

  it("exports components individually and treats only CSS as side-effecting", () => {
    const pkg = json<{
      exports: Record<string, unknown>;
      sideEffects: unknown;
      peerDependencies: Record<string, string>;
      peerDependenciesMeta: Record<string, { optional: boolean }>;
    }>("package.json");

    expect(pkg.exports).toMatchObject({
      "./*": {
        svelte: "./src/*.svelte",
        default: "./src/*.svelte",
      },
    });
    expect(pkg.sideEffects).toEqual(["**/*.css"]);
    expect(pkg.peerDependencies).toMatchObject({
      layerchart: "^2.5.1",
      "@lucide/svelte": "^1.0.1",
    });
    expect(pkg.peerDependenciesMeta).toMatchObject({
      layerchart: { optional: true },
      "@lucide/svelte": { optional: true },
    });
  });

  it("lands component subpaths in the consumer package without a Data barrel", () => {
    const pkg = json<Record<string, unknown>>("package.json");
    const root = json<{ exports: Record<string, unknown> }>("../../package.json");
    const assembled = assemblePackage(
      { name: "@reddb-io/design-system", version: "0.0.0", type: "module" },
      [{ kit: "data", pkg }],
    );

    expect(assembled.exports).toMatchObject({
      "./data/*": {
        svelte: "./kits/data/src/*.svelte",
        default: "./kits/data/src/*.svelte",
      },
    });
    expect(assembled.exports).toHaveProperty("./data/static", { types: "./kits/data/src/static.server.ts", default: "./kits/data/src/static.server.ts" });
    expect(assembled.exports).not.toHaveProperty("./data");
    expect(assembled.sideEffects).toEqual(["./kits/data/**/*.css"]);
    expect(root.exports["./data/*"]).toEqual(
      (assembled.exports as Record<string, unknown>)["./data/*"],
    );
    expect(root.exports).not.toHaveProperty("./data");
  });
});
