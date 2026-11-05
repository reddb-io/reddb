// The Kit ships plain Svelte 5 source with no preprocessor: `lang="ts"` in a
// component is handled by the compiler itself, and styling comes from the
// Theme Layer's utilities rather than from anything that would need a CSS
// pipeline. The config exists so the Svelte plugin stops guessing.
/** @type {import('@sveltejs/vite-plugin-svelte').SvelteConfig} */
export default {};
