<script setup lang="ts">
// Packages page: the space DSM packages hold on the
// selected volume, one row per service line of the volume-level top.list.
// Read-only: package data is freed in the package, never by this tool. Each
// row says how its space is handled; figures that need a caveat live in the
// detail's notes.
import PartialHint from '@/components/PartialHint.vue'
import { reportCompleteness } from '@/components/reportCompleteness'
import { reportNotes } from '@/components/reportNotes'
import ScanNotes from '@/components/ScanNotes.vue'
import ScanBadge from '@/components/ScanBadge.vue'
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import { computed, ref, watch } from 'vue'
import { Badge, Button, CellText, EmptyState, KeyValueRow, ListDetail, LoadingArea } from '@synopackageland/ui'
import type { DataGridColumn } from '@synopackageland/ui'
import { Boxes } from '@lucide/vue'
import { ApiError, api } from '@/api'
import StateView from '@/components/states/StateView.vue'
import type { StateKind } from '@/components/states/states'
import VolumePicker from '@/components/VolumePicker.vue'
import { code, formatBytes, t } from '@/i18n'
import { answersVersion } from '@/stores/answers'
import { navigate, openInExplore, openNewScan, takePackageRequest } from '@/stores/nav'
import { selectedVolume, volumeList, volumes } from '@/stores/volumes'
import type { Path, TopDocument } from '@/types'
import { findRow, handling, howKey, packageRows, type Handling, type PackageRow } from './packages'

/** The volume-level question Explore asks too, so both pages share one cached answer. */
const TOP_LIMIT = 200

const document = ref<TopDocument | null>(null)
const requestError = ref<ApiError | null>(null)
const loading = ref(true)
const expandedKey = ref<string | null>(null)
const clearableCache = ref<number | null>(null)
/** The package an Explore service row asked for, expanded once its row has loaded. */
let pendingRequest = takePackageRequest()
let requestSerial = 0

const volume = computed(() => selectedVolume.value)
const result = computed(() => document.value?.results[0])
const report = computed(() => result.value?.report ?? null)
const rows = computed(() => packageRows(result.value, volume.value?.shares.map(share => share.path.path) ?? []))
const expanded = computed(() => rows.value.find(row => row.key === expandedKey.value) ?? null)
const noReport = computed(() => document.value?.errors.some(error => error.code === 'no_report') ?? false)
const volumeChoices = computed(() => volumeList.value.map(item => ({ value: item.mount.path, label: pathName(item.mount.path) })))
/**
 * The app container's visible height. The detail pane scrolls inside it, so the
 * bound follows a bounded DSM window instead of the (larger) browser viewport.
 */

const completeness = computed(() => reportCompleteness(report.value, document.value?.completion, !!document.value?.errors.length))
const stateKind = computed<StateKind | null>(() => {
  if (requestError.value) {
    if (requestError.value.code === 'no_permission') return 'not_admin'
    if (requestError.value.code === 'backend_unavailable') return 'backend_unavailable'
    return requestError.value.code === 'busy' ? 'busy' : 'error'
  }
  if (!document.value) return null
  if (noReport.value) return 'no_report'
  if (document.value.completion === 'busy') return 'busy'
  if (document.value.completion === 'failed') return 'error'
  return completeness.value.warn ? 'partial' : null
})
const blocking = computed(() => stateKind.value !== null && stateKind.value !== 'partial')
const lastScanSeconds = computed(() => {
  const elapsed = document.value?.scan_hints?.[0]?.last_scan?.elapsed_ms
  return elapsed == null ? null : elapsed / 1000
})

const pageNotes = computed(() => reportNotes(report.value, volume.value?.shares ?? [], document.value?.completion))

const columns = computed<DataGridColumn[]>(() => [
  { key: 'name', label: t('packages.col.package'), minWidth: 160, sortable: true, compare: compareText },
  { key: 'physical', label: t('packages.col.size'), width: '84px', align: 'right', sortable: true, compare: compareBytes },
  { key: 'handling', label: t('packages.col.handling'), width: '136px', sortable: true, compare: compareText },
])

const HANDLING_TONES: Record<Handling, 'success' | 'info' | 'neutral'> = { here: 'success', in_package: 'info', keep: 'neutral' }

const gridRows = computed(() => rows.value.map(row => ({
  key: row.key,
  name: packageName(row),
  physical: row.physical,
  handling: t(`packages.handling.${handling(row.service)}`),
  row,
})))

/** The caveats of one row: a best reading, unread snapshots, unresolved bytes. */
function rowNotes(row: PackageRow) {
  return [
    ...(row.confirmed ? [] : [t('packages.status.unconfirmed.hint')]),
    ...(row.snapshotsNotRead ? [t('packages.detail.notRead', { count: row.snapshotsNotRead })] : []),
    ...(row.unknown ? [t('packages.detail.unknown', { size: formatBytes(row.unknown) })] : []),
  ]
}

function pathName(path: string) {
  return path.replace(/\/$/, '').split('/').at(-1) || path
}

/** The localized name of a service; the CLI's own English name for a code the catalog lacks. */
function packageName(row: PackageRow) {
  const key = `code.service.${row.service}`
  const name = t(key)
  return name === key ? row.package : name
}

/** Where package data kept in a shared folder lies; Explore counts it in that folder too. */
function inShareText(row: PackageRow) {
  return row.sharedFolder ? t('packages.row.inShare', { folder: row.sharedFolder }) : undefined
}

function howText(row: PackageRow) {
  return t(`packages.how.${howKey(row.service)}`, { place: code('manage_in', row.manageInId) })
}

/** The fixed explanation of a service, when the catalog has one. */
function explanationOf(row: PackageRow) {
  const key = `packages.explain.${row.service}`
  const text = t(key)
  return text === key ? null : text
}

function compareText(left: unknown, right: unknown) {
  return String(left).localeCompare(String(right), undefined, { numeric: true, sensitivity: 'base' })
}

function compareBytes(left: unknown, right: unknown) {
  if (left == null && right == null) return 0
  if (left == null) return -1
  if (right == null) return 1
  return Number(left) - Number(right)
}

function toggle(row: PackageRow) {
  expandedKey.value = expandedKey.value === row.key ? null : row.key
}

function showPendingRequest() {
  if (!pendingRequest) return
  expandedKey.value = findRow(rows.value, pendingRequest.service, pendingRequest.path)?.key ?? null
  pendingRequest = null
}

async function load() {
  const mount = volume.value?.mount.path
  if (!mount) {
    loading.value = false
    return
  }
  const serial = ++requestSerial
  loading.value = true
  clearableCache.value = null
  try {
    const answer = await api('top.list', { target: mount, kind: 'all', by: 'reclaimable', limit: TOP_LIMIT })
    if (serial !== requestSerial) return
    document.value = answer
    requestError.value = null
    showPendingRequest()
    if (rows.value.some(row => row.service === 'syno_reclaim')) {
      try {
        const cache = await api('cache.status', { volume: mount })
        if (serial === requestSerial) {
          clearableCache.value = cache.filesystems.reduce((sum, fs) => sum + fs.bytes, 0)
        }
      } catch {
        // Unavailable cache status leaves the clearable size unknown.
      }
    }
  } catch (caught) {
    if (serial !== requestSerial) return
    document.value = null
    requestError.value = caught instanceof ApiError ? caught : new ApiError('unknown')
  } finally {
    if (serial === requestSerial) loading.value = false
  }
}

function changeVolume(path: string) {
  volumes.selected = path
}

watch(() => volume.value?.mount.path, () => {
  expandedKey.value = null
  void load()
}, { immediate: true })
// A scan publish or a finished cleanup changes what the reports describe.
watch(answersVersion, () => load())
</script>

<template>
  <div class="packages-page">
    <ScanBadge v-if="report" :report="report" :consistency="stateKind === 'partial' ? 'partial' : undefined" :notes="pageNotes" />

    <div v-if="volumeChoices.length > 1" class="picker">
      <VolumePicker
        v-bind="{ ariaLabel: t('common.volumePicker.label') }"
        :model-value="volume?.mount.path ?? ''"
        :volumes="volumeChoices"
        @update:model-value="changeVolume"
      />
    </div>

    <div class="body">
      <LoadingArea v-if="loading && !document" :text="t('common.loading')" />

      <StateView v-else-if="stateKind === 'no_report'" kind="no_report" :last-scan-seconds="lastScanSeconds">
        <Button variant="primary" @click="openNewScan">{{ t('packages.scanNow') }}</Button>
      </StateView>

      <StateView v-else-if="blocking && stateKind" :kind="stateKind" :error="requestError">
        <Button v-if="stateKind === 'busy' || stateKind === 'error'" variant="ghost" @click="load">{{ t('common.retry') }}</Button>
      </StateView>

      <template v-else>
        <PartialHint v-if="stateKind === 'partial'" :details="[t('state.partial.body')]" />

        <div class="intro">
          <p>{{ t('packages.intro') }}</p>
          <p class="scope">{{ t('packages.scope') }}</p>
        </div>

        <ListDetail
          storage-key="packages"
          :title="expanded ? packageName(expanded) : undefined"
          @close="expandedKey = null"
        >
          <EmptyState v-if="!rows.length" :title="t('packages.empty.title')" :description="t('packages.empty.body')" :icon="Boxes" />

          <ReclaimGrid
            v-else
            table-id="packages-v2"
            :columns="columns"
            :rows="gridRows"
            row-key="key"
            :selected-keys="expandedKey ? [expandedKey] : []"
            :loading="loading"
            :default-sort="{ key: 'physical', direction: 'desc' }"
            :empty-label="t('packages.empty.title')"
          >
            <template #cell-name="{ row }">
              <button
                type="button"
                class="name-button"
                :aria-expanded="expandedKey === row.key"
                :aria-label="t(expandedKey === row.key ? 'packages.detail.hide' : 'packages.detail.show', { name: row.name as string })"
                @click.stop="toggle((row as { row: PackageRow }).row)"
              >
                <CellText :text="String(row.name)" :secondary="inShareText((row as { row: PackageRow }).row)" wrap>
                  <span class="text-[var(--ui-primary)]">{{ row.name }}</span>
                </CellText>
              </button>
            </template>
            <template #cell-physical="{ row }"><CellText class="tabular-nums" :text="formatBytes(row.physical as number)" /></template>
            <template #cell-handling="{ row }">
              <Badge :tone="HANDLING_TONES[handling((row as { row: PackageRow }).row.service)]">{{ row.handling }}</Badge>
            </template>
          </ReclaimGrid>

          <template v-if="expanded" #detail>
            <section class="how">
              <h3>{{ t('packages.detail.how') }}</h3>
              <p>{{ howText(expanded) }}</p>
              <p v-if="explanationOf(expanded)">{{ explanationOf(expanded) }}</p>
              <template v-if="expanded.service === 'syno_reclaim'">
                <p>{{ t('packages.own.clearable', { size: formatBytes(clearableCache) }) }}</p>
                <Button variant="primary" @click="navigate('history')">{{ code('manage_in', 'syno_reclaim') }}</Button>
              </template>
            </section>
            <div class="figures">
              <KeyValueRow :label="t('packages.col.size')"><span class="tabular-nums">{{ formatBytes(expanded.physical) }}</span></KeyValueRow>
              <KeyValueRow v-if="handling(expanded.service) === 'in_package' && expanded.canFree != null" :label="t('packages.detail.canFree')">
                <span class="tabular-nums">{{ formatBytes(expanded.canFree) }}</span>
              </KeyValueRow>
              <KeyValueRow v-if="expanded.heldBySnapshots" :label="t('packages.detail.snapshots')">
                <span class="tabular-nums">{{ formatBytes(expanded.heldBySnapshots) }}</span>
              </KeyValueRow>
            </div>
            <p v-if="expanded.service !== 'syno_reclaim'" class="never">{{ t('packages.detail.never') }}</p>
            <ScanNotes :notes="rowNotes(expanded)" />
            <details class="technical">
              <summary>{{ t('packages.detail.technical') }}</summary>
              <p class="names">{{ expanded.names }}</p>
              <p class="path-count">{{ t('packages.detail.paths', { count: expanded.pathCount }) }}</p>
              <ul v-if="expanded.paths.length" class="paths">
                <li v-for="path in expanded.paths" :key="path.path_hex ?? path.path">
                  <button
                    type="button"
                    class="path-button"
                    :aria-label="t('packages.detail.open', { path: path.path })"
                    :title="path.path"
                    @click="openInExplore(path as Path)"
                  >
                    {{ path.path }}
                  </button>
                </li>
              </ul>
              <p v-else>{{ t('packages.detail.noPaths') }}</p>
              <p v-if="expanded.pathCount > expanded.paths.length && expanded.paths.length">
                {{ t('packages.detail.firstOnly', { shown: expanded.paths.length, total: expanded.pathCount }) }}
              </p>
            </details>
          </template>
        </ListDetail>
      </template>
    </div>
  </div>
</template>

<style scoped>
.packages-page {
  display: flex;
  height: 100%;
  min-height: 0;
  min-width: 0;
  flex-direction: column;
  gap: var(--ui-section-gap);
}
.picker {
  flex-shrink: 0;
  border-bottom: 1px solid var(--ui-border);
}
.body {
  display: flex;
  flex: 1 1 auto;
  min-height: 0;
  flex-direction: column;
  gap: var(--ui-section-gap);
}
.intro {
  display: grid;
  gap: 4px;
  font-size: var(--ui-font-size);
  color: var(--ui-text);
}
.intro p {
  margin: 0;
}
.intro .scope {
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
.name-button {
  display: flex;
  width: 100%;
  min-width: 0;
  align-items: center;
  gap: 6px;
  border-radius: var(--ui-radius-md);
  text-align: left;
  outline: none;
}
.name-button:focus-visible {
  box-shadow: 0 0 0 2px var(--ui-focus-ring);
}
.how {
  display: grid;
  gap: 6px;
  color: var(--ui-text);
}
.how h3 {
  font-size: var(--ui-font-size);
  font-weight: 600;
}
.how p,
.never,
.technical p {
  margin: 0;
}
.figures {
  display: grid;
}
.never {
  font-size: var(--ui-font-size-sm);
}
.technical {
  display: grid;
  gap: 4px;
  font-size: var(--ui-font-size-sm);
}
.technical summary {
  width: fit-content;
  cursor: pointer;
  border-radius: var(--ui-radius-md);
  outline: none;
}
.technical summary:focus-visible,
.path-button:focus-visible {
  box-shadow: 0 0 0 2px var(--ui-focus-ring);
}
.names {
  overflow-wrap: anywhere;
}
.path-count {
  font-weight: 600;
  color: var(--ui-text);
}
.paths {
  display: grid;
  gap: 4px;
  margin: 0;
  padding: 0;
  list-style: none;
}
.path-button {
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  border-radius: var(--ui-radius-md);
  text-align: left;
  color: var(--ui-primary);
  outline: none;
}
.path-button:hover {
  text-decoration: underline;
}
</style>
