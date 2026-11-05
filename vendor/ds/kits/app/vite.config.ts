import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";
import { componentFlavorResolution } from "../../scripts/flavor-resolution.ts";

export default defineConfig({
  plugins: [componentFlavorResolution(new URL("../..", import.meta.url).pathname), svelte()],
  // The Kit's tests mount components the way a browser does — `mount()` into a
  // real element, then click it — so they need the browser build of Svelte and
  // a document to mount into. That is the difference between a component test
  // and the showcase's smoke test, which renders to a string on the server:
  // only one of them can prove that a disabled Button ignores a click.
  resolve: {
    conditions: ["browser"],
    // The public package subpath is the canonical contract, but this Kit is a
    // private workspace package and pnpm does not link it to its parent root
    // package. Point the specifier at the root package's exact Base export
    // target while keeping every test import at the consumer-facing seam.

  },
  test: {
    include: ["test/**/*.test.ts"],
    environment: "jsdom",
    // Recursive runs execute all three jsdom-heavy Kits together. Keep this
    // Kit to one worker so package-level parallelism cannot multiply into a
    // worker swarm and turn sub-second behavior checks into false timeouts.
    maxWorkers: 1,
  },
});
