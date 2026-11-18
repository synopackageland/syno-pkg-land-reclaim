<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { AppRoot, ToastProvider } from '@synopackageland/ui'
import { locale } from '@/i18n'
import RootfsApplyStep from './RootfsApplyStep.vue'
import HistoryPage from '@/pages/History/HistoryPage.vue'
import { loadVolumes, volumes } from '@/stores/volumes'
import {
  ROOTFS_FIXTURE_SCENARIOS,
  rootfsFixtureScenario,
  selectRootfsFixtureScenario,
} from './rootfsApplyFixtureBackend'

const scenario = ref(rootfsFixtureScenario.value)
const isHistory = computed(() => scenario.value.startsWith('history-'))

function changeScenario(event: Event) {
  const value = (event.target as HTMLSelectElement).value
  if (!ROOTFS_FIXTURE_SCENARIOS.includes(value as typeof ROOTFS_FIXTURE_SCENARIOS[number])) return
  sessionStorage.clear()
  selectRootfsFixtureScenario(value as typeof ROOTFS_FIXTURE_SCENARIOS[number])
  scenario.value = rootfsFixtureScenario.value
}

function returnToSelector() {
  scenario.value = 'moves'
  selectRootfsFixtureScenario('moves')
  sessionStorage.clear()
}

async function loadHistoryFixture() {
  volumes.document = null
  volumes.selected = '/volume1'
  await loadVolumes()
}

onMounted(() => {
  if (isHistory.value) void loadHistoryFixture()
})

watch(scenario, () => {
  if (isHistory.value) void loadHistoryFixture()
})
</script>

<template>
  <AppRoot class="rootfs-fixture-app" :lang="locale">
    <ToastProvider>
      <main>
        <header class="fixture-header">
          <div>
            <p>Development fixture · simulated API · no NAS changes</p>
            <h1>{{ isHistory ? 'History' : 'System partition cleanup' }}</h1>
          </div>
          <label>
            <span>Scenario</span>
            <select :value="scenario" @change="changeScenario">
              <option v-for="entry in ROOTFS_FIXTURE_SCENARIOS" :key="entry" :value="entry">{{ entry }}</option>
            </select>
          </label>
        </header>
        <HistoryPage v-if="isHistory" :key="scenario" />
        <RootfsApplyStep
          v-else
          :key="scenario"
          :open="true"
          :plan-id="'rootfs-plan-fixture-0001'"
          @close="returnToSelector"
        />
      </main>
    </ToastProvider>
  </AppRoot>
</template>

<style>
html, body, #rootfs-fixture { min-height: 100%; margin: 0; }
.rootfs-fixture-app { box-sizing: border-box; min-height: 100vh; padding: 18px 22px; }
.rootfs-fixture-app main { max-width: 1060px; margin: 0 auto; }
.fixture-header { display: flex; align-items: end; justify-content: space-between; gap: 16px; margin-bottom: 20px; }
.fixture-header p { margin-bottom: 6px; color: var(--ui-text-secondary); font-size: var(--ui-font-size-sm); }
.fixture-header h1 { font-size: 20px; font-weight: 700; }
.fixture-header label { display: flex; flex-direction: column; gap: 4px; color: var(--ui-text-secondary); font-size: var(--ui-font-size-sm); }
.fixture-header select { min-width: 180px; padding: 7px; border: 1px solid var(--ui-border); border-radius: var(--ui-radius-md); background: var(--ui-surface); color: var(--ui-text); }
@media (max-width: 640px) { .fixture-header { align-items: flex-start; flex-direction: column; } }
</style>
