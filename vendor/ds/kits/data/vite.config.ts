import { realpathSync } from "node:fs";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";
import { componentFlavorResolution } from "../../scripts/flavor-resolution.ts";

export default defineConfig({
  plugins: [componentFlavorResolution(new URL("../..", import.meta.url).pathname), svelte()],
  resolve: {
    conditions: ["browser"],

  },
  server: { fs: { allow: [new URL("../..", import.meta.url).pathname, realpathSync(new URL("../../node_modules", import.meta.url))] } },
  test: {
    include: ["test/**/*.test.ts"],
    environment: "jsdom",
    setupFiles: ["test/setup.ts"],
    maxWorkers: 1,
  },
});
