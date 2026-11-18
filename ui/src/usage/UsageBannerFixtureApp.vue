<script setup lang="ts">
import { AppRoot } from '@synopackageland/ui'
import { locale } from '@/i18n'
import UsageChangeBanner from './UsageChangeBanner.vue'
</script>

<template>
  <AppRoot class="usage-banner-fixture" :lang="locale">
    <main class="usage-banner-fixture__page">
      <h1>Overview</h1>
      <UsageChangeBanner />
    </main>
  </AppRoot>
</template>

<style scoped>
.usage-banner-fixture { min-height: 100vh; background: var(--ui-bg); }
.usage-banner-fixture__page { padding: 24px; }
.usage-banner-fixture__page h1 { margin: 0; font-size: 20px; }
.usage-banner-fixture :deep(.usage-change) { padding: 16px 0 0; }
</style>
