import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [tailwindcss(), svelte({ configFile: fileURLToPath(new URL('../../svelte.config.js', import.meta.url)) })],
  build: { assetsInlineLimit: 0 },
})
