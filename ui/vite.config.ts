import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'

// The SDK imports the DSM module and keeps optional pages in lazy chunks.
const dsmBundle = {
  define: { 'process.env.NODE_ENV': JSON.stringify('production') },
  build: {
    outDir: 'dist-dsm',
    cssCodeSplit: false,
    lib: {
      entry: fileURLToPath(new URL('./src/dsmModule.ts', import.meta.url)),
      formats: ['es' as const],
      fileName: () => 'app.module.js',
      cssFileName: 'app',
    },
    rollupOptions: {output: {chunkFileNames: 'chunks/[name]-[hash].js'}},
  },
}

const appBundle = {
  build: {
    rollupOptions: {
      output: {
        manualChunks(id: string) {
          if (id.includes('/src/i18n/') && id.includes('.json')) {
            const locale = id.match(/\/src\/i18n\/([^/]+)\//)?.[1]
            return ['zh-TW', 'zh-CN', 'ja', 'ko'].includes(locale ?? '')
              ? 'i18n-east-asian'
              : 'i18n-western'
          }
          if (id.includes('/node_modules/@vue/') || id.includes('/node_modules/vue/')) return 'vue'
          if (id.includes('/node_modules/@vueuse/')) return 'vueuse'
          if (id.includes('/node_modules/@floating-ui/')) return 'floating-ui'
          if (id.includes('/node_modules/@synopackageland/ui/')) return 'synology-ui'
          if (id.includes('/node_modules/@lucide/vue/')) return 'icons'
        },
      },
    },
  },
}

export default defineConfig(({ mode }) => ({
  plugins: [vue()],
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  ...(mode === 'dsm' ? dsmBundle : appBundle),
  test: {
    globals: true,
    // App mounts use jsdom alongside the gate's Rust and SPK builds. Keep
    // one worker so mounted App fixtures do not compete with each other during the gate.
    maxWorkers: 1,
    // Mounted App integration fixtures render alongside cross-architecture builds.
    // This is the runner budget; request and cancellation assertions keep their own clocks.
    testTimeout: 30_000,
    environment: 'jsdom',
    include: ['src/**/*.test.ts', 'build/**/*.test.ts'],
    // jsdom gets no stylesheet otherwise; the styles whose layout rules the
    // tests assert must be listed here (AppShell.test.ts, dialogLayout.test.ts,
    // SettingsPage.test.ts, the page tests that pin a layout).
    css: {
      include: [
        /shell\/AppShell\.vue/,
        /Settings\/SettingsPage\.vue/,
        /Overview\/OverviewPage\.vue/,
        /Explore\/ExplorePage\.vue/,
        /Recycle\/RecyclePage\.vue/,
      ],
    },
  },
}))
