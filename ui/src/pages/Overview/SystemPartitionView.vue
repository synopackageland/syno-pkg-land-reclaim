<script setup lang="ts">
// The System partition view: the Overview body while the System partition
// card is selected. The cleanup dialog runs the
// confirmed rootfs plan/apply flow without changing this view's read model.
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import { computed, ref } from 'vue'
import { Accordion, Badge, Button, Callout, Progress, LoadingArea, Spinner, Surface, TypoA } from '@synopackageland/ui'
import { ChevronRight, CircleCheck, ExternalLink } from '@lucide/vue'
import type { ApiError } from '@/api'
import StateView from '@/components/states/StateView.vue'
import { code, formatBytes, formatCount, formatPercent, formatRelative, locale, t, type Locale } from '@/i18n'
import type { RootfsReportDocument } from '@/types'
import { deepCleanable, ownerRows, reportLevel, smallPartitionNote, stillShort, topHolder, topOwner, type OwnerPart, type OwnerRow } from './rootfsView'
import { UPDATE_HEADROOM_BYTES, UPDATE_ROOM_BYTES } from './systemPartition'
import RootfsCleanupDialog from '@/rootfs/RootfsCleanupDialog.vue'

const props = defineProps<{
  report: RootfsReportDocument | null
  loading: boolean
  /** A walk is checking again behind the shown report. */
  refreshing?: boolean
  error: ApiError | null
}>()
const emit = defineEmits<{ refresh: [] }>()

const MIB = 1_048_576
const PACKAGE_CENTER_APP = 'SYNO.SDS.PkgManApp.Instance'
/** DSM's own update-error article, the only public source for a small partition (§2, Q10). */
const KB_LOCALES: Partial<Record<Locale, string>> = { 'zh-TW': 'zh-tw', 'zh-CN': 'zh-cn' }
const KB_URL = `https://kb.synology.com/${KB_LOCALES[locale] ?? 'en-global'}/DSM/tutorial/DSM_update_insufficient_system_capacity_for_update`

const cleanupDialog = ref<InstanceType<typeof RootfsCleanupDialog>>()
const level = computed(() => (props.report ? reportLevel(props.report) : null))
const tone = computed(() => (level.value === 'full' ? ('danger' as const) : ('warning' as const)))
const partial = computed(() => !!props.report && (!props.report.walk.complete || props.report.completion === 'partial'))
const usedRatio = computed(() => {
  const fs = props.report?.filesystem
  return fs && fs.total_bytes > 0 ? (fs.total_bytes - fs.available_bytes) / fs.total_bytes : 0
})
const standard = computed(() => ((props.report?.cleanable.items ?? 0) > 0 ? props.report!.cleanable : null))
/** Deep mode's totals, shown only when they add to Standard's. */
const deep = computed(() => {
  const totals = props.report ? deepCleanable(props.report) : null
  return totals && totals.bytes > (standard.value?.bytes ?? 0) ? totals : null
})
const movable = computed(() => !!standard.value || !!deep.value)
/** Even the deep cleanup leaves too little room for a DSM update: what is left to try. */
const short = computed(() => (props.report ? stillShort(props.report) : null))
// The shortfall callout links the same article, so the state card's note steps aside.
const small = computed(() => !!props.report && !short.value && smallPartitionNote(props.report))
const logQuota = computed(() => props.report?.log_quota ?? null)
const open = computed(() => props.report?.open_deleted ?? null)
const holder = computed(() => (open.value ? topHolder(open.value.holders) : null))

/** The item cell: a package, a category cleanup moves, DSM or Other (§4 S3). */
function itemText(row: OwnerRow) {
  if (row.kind === 'dsm') return t('rootfsView.row.dsm')
  if (row.kind === 'other') return code('rootfs_category', 'other')
  return categoryText(row.category ?? 'other', row.name)
}

function categoryText(category: string, name: string | null) {
  const key = `code.rootfs_category.${category}`
  return t(key, { name: name ?? '' }) === key ? code('rootfs_category', category) : t(key, { name: name ?? '' })
}

/** A read-only line of the DSM or Other row. */
function partText(part: OwnerPart) {
  if (part.category === 'small') return t('rootfsView.owners.small', { count: part.items ?? 0, below: formatBytes(part.below) })
  if (part.category === 'package_data') return t('rootfsView.part.packageFolders')
  if (part.category === 'other') return t('rootfsView.part.other')
  return categoryText(part.category, part.name)
}

const rows = computed<TableRow[]>(() =>
  (props.report ? ownerRows(props.report) : []).map(owner => ({
    id: owner.id,
    owner,
    item: itemText(owner),
    size: owner.bytes,
    share: owner.percent,
  })),
)
type TableRow = { id: string; owner: OwnerRow; item: string; size: number; share: number }
/** The grid hands its slot the row as `unknown` (see ActionList). */
const rowOf = (value: unknown) => value as TableRow
const folds = (row: TableRow) => row.owner.parts.length > 1

const columns = computed(() => [
  { key: 'item', sortable: true, label: t('rootfsView.owners.col.item') },
  { key: 'size', sortable: true, label: t('rootfsView.owners.col.size'), align: 'right' as const, width: '120px' },
  { key: 'share', sortable: true, label: t('rootfsView.owners.col.share'), align: 'right' as const, width: '90px' },
  { key: 'what', label: t('rootfsView.owners.col.what'), width: '190px' },
])

/** The largest owner when nothing moves; a folded row names its largest part. */
const noneOwner = computed(() => {
  const top = props.report ? topOwner(props.report) : null
  if (!top) return t('common.unknown')
  return top.kind === 'dsm' || top.kind === 'other' ? partText(top.parts[0]) : itemText(top)
})

function openPackageCenter() {
  const syno = window.SYNO?.SDS as { AppLaunch?: (id: string) => void } | undefined
  syno?.AppLaunch?.(PACKAGE_CENTER_APP)
}

function openCleanup() {
  void cleanupDialog.value?.open()
}
</script>

<template>
  <div class="rootfs-view">
    <template v-if="report">
      <Callout v-if="partial" class="rootfs-partial" tone="warning" :title="t('rootfsView.partial')" />
      <Callout v-if="short" class="rootfs-short" tone="warning" :title="t('rootfsView.short.title')">
        <p>{{ t('rootfsView.short.room', { free: formatBytes(short.after), typical: formatBytes(UPDATE_ROOM_BYTES), worst: formatBytes(UPDATE_HEADROOM_BYTES) }) }}</p>
        <ul class="rootfs-short-list">
          <li v-if="short.userSoftware >= MIB">{{ t('rootfsView.short.userSoftware', { bytes: formatBytes(short.userSoftware) }) }}</li>
          <li v-if="short.rootLeft >= MIB">{{ t('rootfsView.short.root', { bytes: formatBytes(short.rootLeft) }) }}</li>
          <li>{{ t('rootfsView.short.sharedFolders') }}</li>
          <li>{{ t('rootfsView.short.systemLog') }}</li>
          <li v-if="short.heldOpen > 0">{{ t('rootfsView.short.restart', { bytes: formatBytes(short.heldOpen) }) }}</li>
        </ul>
        <p>
          {{ t('rootfsView.short.article', { total: formatBytes(report.filesystem.total_bytes) }) }}
          <TypoA :href="KB_URL" target="_blank" rel="noopener">{{ t('rootfsView.usage.learnMore') }}</TypoA>
        </p>
      </Callout>

      <div class="rootfs-top">
        <Surface class="rootfs-state">
          <span class="rootfs-state-head">
            <span class="rootfs-state-name">{{ t('rootfs.name') }}</span>
            <Badge :tone="tone">{{ level ? t(`rootfs.level.${level}`) : t('common.unknown') }}</Badge>
          </span>
          <span v-if="level" class="rootfs-state-body">{{ t(`rootfs.level.${level}.body`) }}</span>
          <Progress class="rootfs-bar" :value="usedRatio * 100" :tone="tone" :aria-label="t('rootfs.name')" />
          <span class="rootfs-usage">
            <span>{{ t('rootfsView.usage.line', { used: formatBytes(report.filesystem.used_bytes), total: formatBytes(report.filesystem.total_bytes), free: formatBytes(report.filesystem.available_bytes) }) }}</span>
            <span class="num">{{ t('rootfsView.usage.percent', { percent: formatPercent(Math.round(usedRatio * 100) / 100) }) }}</span>
          </span>
          <span class="muted">{{ t('rootfsView.usage.guide', { typical: formatBytes(UPDATE_ROOM_BYTES), worst: formatBytes(UPDATE_HEADROOM_BYTES) }) }}</span>
          <span v-if="small" class="muted">
            {{ t('rootfsView.usage.small') }}
            <TypoA :href="KB_URL" target="_blank" rel="noopener">{{ t('rootfsView.usage.learnMore') }}</TypoA>
          </span>
        </Surface>

        <Surface class="rootfs-movable">
          <span v-if="movable" class="rootfs-movable-title">{{ t('rootfsView.movable.title') }}</span>
          <template v-if="movable">
            <template v-if="standard">
              <span v-if="deep" class="rootfs-movable-mode">{{ t('rootfsView.movable.standard') }}</span>
              <strong class="rootfs-movable-bytes num">{{ formatBytes(standard.bytes) }}</strong>
              <ul class="rootfs-movable-list">
                <li v-for="entry in standard.by_category" :key="entry.category">
                  <span>{{ code('rootfs_category', entry.category) }}</span>
                  <span class="num">{{ formatBytes(entry.bytes) }}</span>
                </li>
              </ul>
            </template>
            <template v-if="deep">
              <span class="rootfs-movable-mode">{{ t('rootfsView.movable.deep', { bytes: formatBytes(deep.bytes) }) }}</span>
              <ul class="rootfs-movable-list">
                <li v-for="entry in deep.by_category" :key="entry.category">
                  <span>{{ code('rootfs_category', entry.category) }}</span>
                  <span class="num">{{ formatBytes(entry.bytes) }}</span>
                </li>
              </ul>
            </template>
            <span class="muted">{{ t('rootfsView.movable.tools') }}</span>
          </template>
          <template v-else>
            <span class="rootfs-movable-none">{{ t('rootfsView.movable.none', { owner: noneOwner }) }}</span>
          </template>
          <span v-if="movable" class="muted">{{ t('rootfsView.movable.body') }}</span>
          <Button v-if="movable" class="rootfs-movable-button" size="sm" variant="primary" @click="openCleanup">
            {{ t('rootfsView.cleanUp') }}
          </Button>
        </Surface>
      </div>

      <div class="rootfs-owners-head">
        <h2>{{ t('rootfsView.owners.title') }}</h2>
        <span class="muted">{{ t('rootfsView.checked', { time: formatRelative(report.generated_at) }) }}</span>
        <span v-if="refreshing" class="rootfs-refreshing muted" role="status">
          <Spinner inline :size="12" />{{ t('rootfsView.refreshing') }}
        </span>
        <span v-else-if="error" class="rootfs-refresh-failed" role="status">{{ t('rootfsView.refreshFailed') }}</span>
        <Button class="rootfs-refresh" size="sm" variant="ghost" :disabled="refreshing" @click="emit('refresh')">{{ t('rootfsView.refresh') }}</Button>
      </div>
      <ReclaimGrid
        :fill="false"
        table-id="rootfs-owners"
        :columns="columns"
        :rows="rows"
        :empty-label="t('common.unknown')"
        :expandable="{ type: 'cell' }"
      >
        <template #cell-item="{ row, value, expanded, toggleExpand }">
          <button v-if="folds(rowOf(row))" type="button" class="rootfs-fold" :aria-expanded="expanded" @click="toggleExpand">
            <ChevronRight class="rootfs-fold-icon size-3.5" :class="{ 'rotate-90': expanded }" />{{ value }}
          </button>
          <template v-else>{{ value }}</template>
        </template>
        <template #cell-size="{ row }">{{ formatBytes(rowOf(row).size) }}</template>
        <template #cell-share="{ row }"><span class="num">{{ formatPercent(rowOf(row).share / 100) }}</span></template>
        <template #cell-what="{ row }">
          <span v-if="rowOf(row).owner.kind === 'cleanable'" class="rootfs-mark"><CircleCheck class="size-4" />{{ t('rootfsView.owners.movable') }}</span>
          <button v-else-if="rowOf(row).owner.kind === 'package'" type="button" class="rootfs-link" @click="openPackageCenter">
            {{ t('rootfsView.what.package_data') }}<ExternalLink class="size-3.5" />
          </button>
        </template>
        <template #expand="{ row }">
          <div class="rootfs-parts">
            <span class="muted">{{ t(rowOf(row).owner.kind === 'dsm' ? 'rootfsView.dsm.note' : 'rootfsView.other.note') }}</span>
            <ul>
              <li v-for="part in rowOf(row).owner.parts" :key="part.id">
                <span>{{ partText(part) }}</span>
                <span class="num">{{ formatBytes(part.bytes) }}</span>
              </li>
            </ul>
          </div>
        </template>
      </ReclaimGrid>

      <Accordion :items="[{ value: 'details', title: t('rootfsView.details') }]">
        <template #item-details>
          <ul class="rootfs-details">
            <li v-if="logQuota">
              {{ t('rootfsView.details.logs', { used: formatBytes(logQuota.used_bytes), limit: formatBytes(logQuota.hard_limit_bytes) }) }}
            </li>
            <li v-if="logQuota && logQuota.used_bytes >= logQuota.soft_limit_bytes" class="rootfs-warn">{{ t('rootfsView.details.logsNear') }}</li>
            <li>{{ t('rootfsView.details.files', { used: formatCount(report.filesystem.inodes_used), total: formatCount(report.filesystem.inodes_total) }) }}</li>
            <li v-if="open && open.bytes_estimate > 0">
              {{
                holder
                  ? t('rootfsView.details.open', { bytes: formatBytes(open.bytes_estimate), program: holder.comm })
                  : t('rootfsView.details.openNoProgram', { bytes: formatBytes(open.bytes_estimate) })
              }}
            </li>
          </ul>
        </template>
      </Accordion>
    </template>

    <LoadingArea v-else-if="loading" :text="t('rootfsView.checking')" />
    <StateView v-else-if="error?.code === 'busy'" kind="busy">
      <Button size="sm" @click="emit('refresh')">{{ t('common.retry') }}</Button>
    </StateView>
    <StateView v-else-if="error" kind="error" :error="error">
      <Button size="sm" @click="emit('refresh')">{{ t('common.retry') }}</Button>
    </StateView>
    <LoadingArea v-else :text="t('rootfsView.checking')" />

    <RootfsCleanupDialog ref="cleanupDialog" @complete="emit('refresh')" @refresh="emit('refresh')" />
  </div>
</template>

<style scoped>
.rootfs-view {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.rootfs-top {
  display: grid;
  grid-template-columns: minmax(360px, 1.25fr) minmax(320px, 1.3fr);
  gap: 16px;
}
.rootfs-state {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.rootfs-state-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.rootfs-state-name,
.rootfs-movable-title {
  font-weight: 600;
}
.rootfs-state-body {
  font-size: var(--ui-font-size);
}
.rootfs-bar {
  margin-top: 4px;
}
.rootfs-usage {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
.rootfs-usage > :last-child {
  white-space: nowrap;
}
.rootfs-movable {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.rootfs-movable-bytes {
  font-size: 28px;
  line-height: 1.1;
}
.rootfs-movable-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: var(--ui-font-size);
}
.rootfs-movable-list li {
  display: flex;
  justify-content: space-between;
  gap: 12px;
}
.rootfs-movable-mode {
  font-size: var(--ui-font-size);
  font-weight: 600;
  margin-top: 4px;
}
.rootfs-short-list {
  list-style: disc;
  padding-left: 20px;
  margin: 4px 0;
}
.rootfs-movable-none {
  font-size: var(--ui-font-size);
}
.rootfs-movable-button {
  align-self: flex-end;
}
.rootfs-owners-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
}
.rootfs-owners-head h2 {
  font-size: var(--ui-font-size-lg);
  font-weight: 600;
}
.rootfs-refresh {
  margin-left: auto;
}
.rootfs-refreshing {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.rootfs-refresh-failed {
  font-size: var(--ui-font-size-sm);
  color: var(--ui-warning);
}
.rootfs-fold {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  background: none;
  border: 0;
  padding: 0;
  color: inherit;
  font: inherit;
  cursor: pointer;
}
.rootfs-fold-icon {
  transition: transform 120ms ease;
}
.rootfs-parts {
  display: flex;
  flex-direction: column;
  gap: 6px;
  /* Part sizes line up with the Size column: clear of Share (90px) and What to do (190px). */
  padding: 4px 288px 4px 18px;
}
.rootfs-parts ul {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: var(--ui-font-size);
}
.rootfs-parts li {
  display: flex;
  justify-content: space-between;
  gap: 12px;
}
.rootfs-mark,
.rootfs-link {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: var(--ui-font-size);
  color: var(--ui-success);
}
.rootfs-link {
  color: var(--ui-primary);
  background: none;
  border: 0;
  padding: 0;
  cursor: pointer;
}
.rootfs-details {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.rootfs-warn {
  color: var(--ui-warning);
}
.num {
  font-variant-numeric: tabular-nums;
}
.muted {
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-muted);
}
</style>
