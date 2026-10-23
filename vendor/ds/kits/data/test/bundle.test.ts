/**
 * @vitest-environment node
 */

import { join } from "node:path";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { beforeAll, describe, expect, it } from "vitest";
import { build, type Rolldown } from "vite";

const KIT_ROOT = join(import.meta.dirname, "..");
const OPTIONAL_PEERS = [
  "layerchart",
  "@lucide/svelte",
  "sigma",
  "graphology",
  "graphology-layout-forceatlas2",
  "@aflsolutions/graphology-communities-leiden",
] as const;
const BUTTON_MARKER = "aria-disabled:pointer-events-none aria-disabled:opacity-50";
const BUILD_TIMEOUT = 180_000;

async function bundle(entry: string): Promise<string> {
  const result = await build({
    root: KIT_ROOT,
    configFile: false,
    logLevel: "silent",
    plugins: [svelte()],
    resolve: {
      conditions: ["browser"],
      alias: [
        { find: /^@reddb-io\/design-system\/base\/recipes$/, replacement: join(KIT_ROOT, "..", "base", "src", "recipes.ts") },
        { find: /^@reddb-io\/design-system\/base$/, replacement: join(KIT_ROOT, "..", "base", "src", "index.ts") },
      ],
    },
    build: {
      write: false,
      minify: false,
      lib: { entry, formats: ["es"], fileName: "bundle" },
      rolldownOptions: {
        external: [/^svelte(\/|$)/, /^layerchart(\/|$)/, /^@lucide\/svelte(\/|$)/],
      },
    },
  });

  const outputs = (Array.isArray(result) ? result[0]! : result) as Rolldown.RolldownOutput;
  return outputs.output
    .filter((chunk): chunk is Rolldown.OutputChunk => chunk.type === "chunk")
    .map((chunk) => chunk.code)
    .join("\n");
}

function optionalPeerLeaks(output: string): string[] {
  return OPTIONAL_PEERS.filter(
    (peer) =>
      output.includes(`"${peer}"`) ||
      output.includes(`'${peer}'`) ||
      output.includes(`"${peer}/`) ||
      output.includes(`'${peer}/`),
  );
}

let consumerBundle = "";
let leakingBundle = "";
let kpiBundle = "";
let chartBundle = "";
let tableBundle = "";

beforeAll(async () => {
  consumerBundle = await bundle(join("test", "fixtures", "one-component.ts"));
  leakingBundle = await bundle(join("test", "fixtures", "optional-peer-leak.ts"));
  kpiBundle = await bundle(join("test", "fixtures", "kpi-card-only.ts"));
  chartBundle = await bundle(join("test", "fixtures", "time-series-only.ts"));
  tableBundle = await bundle(join("test", "fixtures", "table-only.ts"));
}, BUILD_TIMEOUT);

describe("a Data Kit consumer importing one inherited component", () => {
  it("ships the component and zero bytes naming either optional peer", () => {
    expect(consumerBundle.length).toBeGreaterThan(1_000);
    expect(consumerBundle).toContain(BUTTON_MARKER);
    expect(optionalPeerLeaks(consumerBundle)).toEqual([]);
  });

  it("makes either optional peer visible when a component leaks it", () => {
    expect(optionalPeerLeaks(leakingBundle)).toEqual(["layerchart", "@lucide/svelte"]);
  });
});

describe("the chart engine bundle boundary", () => {
  it("keeps LayerChart out of a consumer importing only KpiCard", () => {
    expect(kpiBundle).toContain("data-kpi-card");
    expect(optionalPeerLeaks(kpiBundle)).toEqual([]);
  });

  it("keeps LayerChart explicit when a consumer imports a chart", () => {
    expect(chartBundle).toContain("data-ds-chart");
    expect(optionalPeerLeaks(chartBundle)).toEqual(["layerchart"]);
  });
});

describe("the graph engine bundle boundary", () => {
  it("keeps sigma and graphology out of a consumer importing only Table", () => {
    expect(tableBundle).toContain("data-table");
    expect(optionalPeerLeaks(tableBundle)).toEqual([]);
  });
});
