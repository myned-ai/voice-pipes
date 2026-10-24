/// <reference types="vitest" />
import { defineConfig } from 'vite'
import type { UserConfig } from 'vite'
import type { InlineConfig } from 'vitest/node'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// Vitest's `declare module 'vite'` augmentation (which adds `test` to
// `UserConfig`) lands on the nested copy of vite that vitest resolves, not on
// the top-level vite these plugins are typed against.  Declaring the config
// with an explicit `UserConfig & { test }` type keeps both halves typed
// without a cast: plugins against vite, `test` against vitest.
const config: UserConfig & { test: InlineConfig } = {
  plugins: [react(), tailwindcss()],
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test-setup.ts'],
  },
}

// https://vite.dev/config/
export default defineConfig(config)
