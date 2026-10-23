import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";
import { componentFlavorResolution } from "../../scripts/flavor-resolution.ts";

export default defineConfig({
  plugins: [componentFlavorResolution(new URL("../..", import.meta.url).pathname), svelte()],
  // The Kit's tests mount the Logo the way a browser does — `mount()` into a
  // real element — so they need the browser build of Svelte and a document to
  // mount into. The document is not incidental here: `on` defaults from the
  // active Theme, which is an attribute on the document root, so a test that
  // could not switch Themes could not test the default at all.
  resolve: {
    conditions: ["browser"],
    // Exercise the package subpath used by a Product Application while the
    // Kit is tested in its private workspace package. The root package is the
    // published boundary, but pnpm does not link a workspace package to its
    // own parent package; this alias points at the exact export target declared
    // by the root package.json rather than bypassing the Base entry point.

  },
  test: {
    include: ["test/**/*.test.ts"],
    environment: "jsdom",
    setupFiles: ["test/setup-storage.ts"],
    // Recursive runs execute all three jsdom-heavy Kits together. One worker
    // here keeps workspace parallelism from multiplying into false timeouts.
    maxWorkers: 1,
  },
});
