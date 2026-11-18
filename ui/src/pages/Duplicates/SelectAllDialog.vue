<script setup lang="ts">
// Select all, as a modal: while the groups are read and checked it shows how
// many of how many and can be stopped; then it says what was selected and
// lists each group that could not be, and each copy that stays, with why. It
// informs only: what could be selected already is.
import { Button, Dialog, LoadingArea } from '@synopackageland/ui'
import { formatCount, t } from '@/i18n'

export type SelectAllSkip = { id: string; name: string; why: string }
export type SelectAllStay = { id: string; path: string; why: string }
export type SelectAllState = {
  phase: 'checking' | 'done' | 'stopped'
  done: number
  /** Null until the first page says how many groups there are. */
  total: number | null
  selected: number
  skipped: SelectAllSkip[]
  stays: SelectAllStay[]
}

defineProps<{ state: SelectAllState }>()
const open = defineModel<boolean>('open', { default: false })
const emit = defineEmits<{ stop: [] }>()
</script>

<template>
  <Dialog v-model:open="open" :title="t('duplicates.selectAll.title')" :close-label="t('common.close')" aria-describedby="select-all-status">
    <div class="select-all" data-testid="select-all-dialog">
      <div v-if="state.phase === 'checking'" id="select-all-status" data-testid="select-all-checking">
        <LoadingArea :text="state.total === null ? t('duplicates.selectAll.checking') : t('duplicates.selectAll.progress', { done: formatCount(state.done), total: formatCount(state.total) })" />
      </div>
      <template v-else>
        <p id="select-all-status" class="select-all__lead" data-testid="select-all-result">
          {{ t('duplicates.selectAll.selected', { count: state.selected }) }}
        </p>
        <p v-if="state.phase === 'stopped'" class="select-all__muted" data-testid="select-all-stopped">{{ t('duplicates.selectAll.stopped') }}</p>
        <template v-if="state.skipped.length">
          <h3 class="select-all__section">{{ t('duplicates.selectAll.skipped', { count: state.skipped.length }) }}</h3>
          <ul class="select-all__list" data-testid="select-all-skipped">
            <li v-for="skip in state.skipped" :key="skip.id">
              <span class="select-all__text"><span class="select-all__name">{{ skip.name }}</span><small>{{ skip.why }}</small></span>
            </li>
          </ul>
        </template>
        <template v-if="state.stays.length">
          <h3 class="select-all__section">{{ t('duplicates.selectAll.stays') }}</h3>
          <ul class="select-all__list" data-testid="select-all-stays">
            <li v-for="stay in state.stays" :key="stay.id">
              <span class="select-all__text"><span class="select-all__name">{{ stay.path }}</span><small>{{ stay.why }}</small></span>
            </li>
          </ul>
        </template>
      </template>
    </div>
    <template #footer>
      <div class="select-all__footer">
        <Button v-if="state.phase === 'checking'" variant="secondary" data-testid="select-all-stop" @click="emit('stop')">{{ t('common.cancel') }}</Button>
        <Button v-else variant="primary" data-testid="select-all-close" @click="open = false">{{ t('common.close') }}</Button>
      </div>
    </template>
  </Dialog>
</template>

<style scoped>
.select-all {
  display: grid;
  gap: 10px;
  min-width: 0;
}
.select-all__lead {
  margin: 0;
  font-weight: 600;
}
.select-all__section {
  margin: 4px 0 0;
  color: var(--ui-text-secondary);
  font-size: 12px;
  font-weight: 600;
}
.select-all__muted {
  margin: 0;
  color: var(--ui-text-secondary);
}
.select-all__list {
  display: grid;
  gap: 6px;
  max-height: 320px;
  margin: 0;
  padding: 0;
  overflow: auto;
  list-style: none;
}
.select-all__list li {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}
.select-all__text {
  display: grid;
  flex: 1 1 220px;
  min-width: 0;
}
.select-all__name {
  overflow-wrap: anywhere;
}
.select-all__text small {
  color: var(--ui-text-secondary);
}
.select-all__footer {
  display: flex;
  justify-content: flex-end;
  width: 100%;
}
</style>
