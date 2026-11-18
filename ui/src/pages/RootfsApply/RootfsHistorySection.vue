<script setup lang="ts">
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import { computed, ref } from 'vue'
import { Badge, Button, Callout, CellStatus, CellText, Dialog, Statistic, Surface, Tooltip, useToast, type DataGridColumn } from '@synopackageland/ui'
import { fileStationBlock, openInFileStation } from '@/components/fileStation'
import { formatBytes, formatCount, formatDateTime, joinParts, locale, t } from '@/i18n'
import { formatHistoryDateTime, rootfsOutcomeKey, rootfsToolKey } from './rootfsApply'
import type { RootfsHistoryRecord } from './rootfsTypes'

const props = defineProps<{
  applies: RootfsHistoryRecord[]
  canCleanUp: boolean
}>()
const emit = defineEmits<{ 'check-and-clean-up': [] }>()

const selected = ref<RootfsHistoryRecord | null>(null)
const selectedOpen = computed({
  get: () => selected.value != null,
  set: (open: boolean) => { if (!open) selected.value = null },
})
const columns = computed<DataGridColumn[]>(() => [
  { key: 'when', sortable: true, label: t('rootfsApply.history.when'), width: '17%' },
  { key: 'by', sortable: true, label: t('rootfsApply.history.by'), width: '7%' },
  { key: 'moved', sortable: true, compare: (_a, _b, a, b) => record(a).moved.size_bytes - record(b).moved.size_bytes, label: t('rootfsApply.history.moved'), width: '12%' },
  { key: 'gained', sortable: true, label: t('rootfsApply.history.gained'), width: '16%' },
  { key: 'to', sortable: true, label: t('rootfsApply.history.to'), width: '22%' },
  { key: 'result', sortable: true, label: t('rootfsApply.history.result'), width: '14%' },
  { key: 'actions', label: '', width: '12%', align: 'right' },
])
const rows = computed(() => [...props.applies]
  .sort((a, b) => Date.parse(b.finished) - Date.parse(a.finished))
  .map(apply => ({ ...apply, id: apply.plan_id, when: apply.finished, by: apply.requested_by, gained: apply.freed_bytes, to: apply.move_dir.path, result: apply.completion })))
const dialogTitle = computed(() => selected.value
  ? t('rootfsApply.history.detail.title', { time: formatDateTime(selected.value.finished) })
  : t('rootfsApply.history.title'))

function record(row: Record<string, unknown>) {
  return row as unknown as RootfsHistoryRecord
}

function folderLabel(path: string) {
  return path.split('/').filter(Boolean).slice(-2).join(' ▸ ')
}

function outcomeLabel(outcome: string) {
  return t(rootfsOutcomeKey(outcome), { outcome })
}

function outcomeTone(outcome: string) {
  if (outcome === 'complete' || outcome === 'completed') return 'success'
  if (outcome === 'partial' || outcome === 'paused') return 'warning'
  return 'danger'
}

const toast = useToast()

/** File Station shows only shared folders; a move folder elsewhere is not offered. */
function canOpenMoveFolder(path: string) {
  return fileStationBlock({ path, kind: 'dir' }) == null
}

function openMoveFolder(path: string) {
  if (!openInFileStation({ path, kind: 'dir' })) toast.info(t('common.fileStation.failed'))
}

function checkAndCleanUp() {
  selected.value = null
  emit('check-and-clean-up')
}
</script>

<template>
  <section v-if="applies.length" class="rootfs-history" aria-labelledby="rootfs-history-title">
    <h2 id="rootfs-history-title">{{ t('rootfsApply.history.title') }}</h2>
    <Surface padding="none">
      <ReclaimGrid
        :fill="false"
        table-id="rootfs-history"
        class="rootfs-history-grid"
        :columns="columns"
        :rows="rows"
        row-key="id"
      >
        <template #cell-when="{ row }">{{ formatHistoryDateTime(record(row).finished, locale) }}</template>
        <template #cell-by="{ row }">{{ record(row).requested_by ?? '—' }}</template>
        <template #cell-moved="{ row }">
          <CellText :text="joinParts([formatCount(record(row).moved.files), formatBytes(record(row).moved.size_bytes)], locale)" />
        </template>
        <template #cell-gained="{ row }">{{ formatBytes(record(row).freed_bytes) }}</template>
        <template #cell-to="{ row }">
          <button v-if="canOpenMoveFolder(record(row).move_dir.path)" type="button" class="folder-link" :title="record(row).move_dir.path" @click="openMoveFolder(record(row).move_dir.path)">
            <CellText><span class="text-[var(--ui-primary)] underline">{{ folderLabel(record(row).move_dir.path) }}</span></CellText>
          </button>
          <Tooltip v-else :content="t('common.fileStation.notShared')">
            <span class="folder-link" tabindex="0" :aria-label="record(row).move_dir.path"><CellText :text="folderLabel(record(row).move_dir.path)" /></span>
          </Tooltip>
        </template>
        <template #cell-result="{ row }">
          <CellStatus :tone="outcomeTone(record(row).completion)" :label="outcomeLabel(record(row).completion)" />
        </template>
        <template #cell-actions="{ row }">
          <Button size="sm" variant="ghost" @click="selected = record(row)">
            {{ t('rootfsApply.history.view') }}
          </Button>
        </template>
      </ReclaimGrid>
    </Surface>
    <p class="history-note">{{ t('rootfsApply.history.note') }}</p>

    <Dialog v-model:open="selectedOpen" :aria-describedby="undefined" :title="dialogTitle" :close-label="t('common.close')">
      <template v-if="selected">
        <p>{{ t(`rootfsClean.mode.${selected.mode ?? 'standard'}`) }}</p>
        <div class="detail-stats">
          <Surface class="detail-stat"><Statistic :label="t('rootfsApply.history.gained')" :value="formatBytes(selected.freed_bytes)" /></Surface>
          <Surface class="detail-stat"><Statistic :label="t('rootfsApply.history.moved')" :value="joinParts([formatBytes(selected.moved.size_bytes), formatCount(selected.moved.files)], locale)" /></Surface>
        </div>
        <Surface class="detail-card">
          <h3>{{ t('rootfsApply.history.to') }}</h3>
          <code class="full-path">{{ selected.move_dir.path }}</code>
          <p>{{ t('rootfsApply.result.readme') }}</p>
          <Tooltip :content="t('common.fileStation.notShared')" :disabled="canOpenMoveFolder(selected.move_dir.path)">
            <span class="file-station-action" :tabindex="canOpenMoveFolder(selected.move_dir.path) ? undefined : 0">
              <Button size="sm" variant="ghost" :disabled="!canOpenMoveFolder(selected.move_dir.path)" @click="openMoveFolder(selected.move_dir.path)">{{ t('rootfsApply.result.open') }}</Button>
            </span>
          </Tooltip>
        </Surface>
        <Surface v-if="selected.skipped_count > 0" class="detail-card">
          <h3>{{ t('rootfsApply.history.skipped') }}</h3>
          <p>{{ t('rootfsApply.history.skippedCount', { count: formatCount(selected.skipped_count) }) }}</p>
        </Surface>
        <Surface v-if="selected.tools.length" class="detail-card">
          <h3>{{ t('rootfsApply.result.dsmCleanup') }}</h3>
          <ul class="tool-list">
            <li v-for="tool in selected.tools" :key="tool.tool">
              <span>{{ t(rootfsToolKey(tool.tool), { tool: tool.tool }) }}</span>
              <Badge :tone="tool.outcome === 'succeeded' ? 'success' : tool.outcome === 'not_needed' ? 'neutral' : 'warning'">
                {{ t(rootfsOutcomeKey(tool.outcome), { outcome: tool.outcome }) }}
              </Badge>
            </li>
          </ul>
        </Surface>
        <Callout tone="info">{{ t('rootfsApply.history.note') }}</Callout>
      </template>
      <template #footer>
        <Button v-if="canCleanUp" variant="primary" @click="checkAndCleanUp">{{ t('rootfsApply.history.checkAndCleanUp') }}</Button>
        <Button @click="selected = null">{{ t('common.close') }}</Button>
      </template>
    </Dialog>
  </section>
</template>

<style scoped>
.rootfs-history { display: flex; flex-direction: column; gap: 10px; min-width: 0; }
.rootfs-history h2 { font-size: var(--ui-font-size-lg); font-weight: 700; }
.history-note { color: var(--ui-text-secondary); font-size: var(--ui-font-size-sm); }
.rootfs-history-grid :deep(th), .rootfs-history-grid :deep(td) { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.file-station-action { display: inline-flex; border-radius: var(--ui-radius-sm); outline: none; }
.file-station-action:focus-visible { box-shadow: 0 0 0 2px var(--ui-focus-ring); }
.folder-link { display: block; width: 100%; max-width: 180px; text-align: left; }
.detail-stats { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; margin-bottom: 12px; }
.detail-stat { padding: 14px; }
.detail-card { margin-bottom: 12px; padding: 14px; }
.detail-card h3 { margin-bottom: 8px; font-size: var(--ui-font-size); font-weight: 700; }
.full-path { display: block; overflow-wrap: anywhere; margin-bottom: 8px; font-size: var(--ui-font-size-sm); }
.detail-card p { margin-bottom: 8px; color: var(--ui-text-secondary); font-size: var(--ui-font-size-sm); }
.tool-list { display: flex; flex-direction: column; gap: 7px; padding: 0; margin: 0; list-style: none; }
.tool-list li { display: flex; align-items: center; justify-content: space-between; gap: 10px; font-size: var(--ui-font-size-sm); }
@media (max-width: 800px) { .detail-stats { grid-template-columns: 1fr; } }
</style>
