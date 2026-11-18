<script setup lang="ts">
// Dev server only: picks the fixture scenario (ui/fixtures/README.md).
import { Select } from '@synopackageland/ui'
import { SCENARIOS, scenarioFromUrl } from '@/api/fixtures'
import { t } from '@/i18n'

const options = SCENARIOS.map(value => ({ label: value, value }))

function choose(value: string) {
  const url = new URL(window.location.href)
  url.searchParams.set('scenario', value)
  window.location.assign(url)
}
</script>

<template>
  <!-- A fixed width: the header sizes its actions before a Select resolves its min-width. -->
  <Select style="width: 8rem" :model-value="scenarioFromUrl()" :options="options" :aria-label="t('shell.scenario')" @update:model-value="choose(String($event))" />
</template>
