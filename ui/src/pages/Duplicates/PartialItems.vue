<script setup lang="ts">
// Partly identical folders, expanded: the identical items a cleanup removes
// from each removed side, largest first, a page at a time. The side folders
// themselves and the items that differ (.git of a clone) stay; the note says so.
import { computed, onMounted, ref, shallowRef, watch } from 'vue'
import { Button, DetailList, type DetailListItem } from '@synopackageland/ui'
import { File, Folder } from '@lucide/vue'
import { formatBytes, formatCount, t } from '@/i18n'
import type { DuplicateGroup, DuplicateItem } from '@/types/duplicates'
import { ITEMS_LIMIT, partialItems } from './duplicatesClient'
import { rowName } from './duplicatesModel'

type Line = DetailListItem & { kind: DuplicateItem['kind'] }

const props = defineProps<{
  group: DuplicateGroup
  volume: string
  /** The scope folder the API gets; the volume when left out. */
  scopePath?: string
}>()

const items = shallowRef<DuplicateItem[]>([])
const total = ref(0)
const loading = ref(false)
const failed = ref(false)
let request = 0

async function load(more = false) {
  const id = ++request
  loading.value = true
  failed.value = false
  try {
    const page = await partialItems({ volume: props.volume, group_id: props.group.group.id, scope: props.scopePath, offset: more ? items.value.length : 0 })
    if (id !== request) return
    items.value = more ? [...items.value, ...page.items] : page.items
    total.value = page.total
  } catch {
    if (id === request) failed.value = true
  } finally {
    if (id === request) loading.value = false
  }
}
onMounted(() => void load())
watch(() => [props.group.group.id, props.scopePath, props.volume], () => void load())

const name = computed(() => rowName(props.group))
const count = computed(() => props.group.group.same_items ?? props.group.partial?.same_items ?? total.value)
const lines = computed<Line[]>(() => items.value.map(item => ({
  key: item.path.path,
  kind: item.kind,
  text: item.kind === 'folder' ? `${item.path.path}/` : item.path.path,
  secondary: item.kind === 'folder'
    ? t('duplicates.partial.folderItem', { count: item.files, files: formatCount(item.files), size: formatBytes(item.bytes) })
    : formatBytes(item.bytes),
})))
const remaining = computed(() => Math.max(0, total.value - items.value.length))
</script>

<template>
  <section class="partial-items" data-testid="partial-items">
    <p class="partial-items__title">{{ t('duplicates.partial.title', { count, items: formatCount(count), name }) }}</p>
    <p class="partial-items__note" data-testid="partial-stays">{{ t('duplicates.partial.stays', { name }) }}</p>
    <p v-if="failed" class="partial-items__note" role="alert">
      {{ t('duplicates.partial.failed') }}
      <Button size="sm" variant="ghost" @click="load(items.length > 0)">{{ t('duplicates.partial.retry') }}</Button>
    </p>
    <DetailList
      v-if="lines.length"
      :items="lines"
      :max="lines.length"
      :summary="remaining ? t('duplicates.partial.more', { count: remaining }) : undefined"
      :label="t('duplicates.partial.label', { name })"
    >
      <template #leading="{ item }">
        <component :is="(item as Line).kind === 'folder' ? Folder : File" class="partial-items__icon" aria-hidden="true" />
      </template>
      <template v-if="remaining" #summary-trailing>
        <Button size="sm" variant="ghost" :loading="loading" data-testid="partial-more" @click="load(true)">{{ t('duplicates.more', { count: Math.min(remaining, ITEMS_LIMIT) }) }}</Button>
      </template>
    </DetailList>
  </section>
</template>

<style scoped>
.partial-items {
  margin-top: 8px;
}
.partial-items__title {
  margin: 0;
  font-size: var(--ui-font-size);
  font-weight: 600;
  color: var(--ui-text);
}
.partial-items__note {
  margin: 2px 0 6px;
  font-size: 12px;
  color: var(--ui-text-secondary);
}
.partial-items__icon {
  width: 14px;
  height: 14px;
  color: var(--ui-text-secondary);
}
</style>
