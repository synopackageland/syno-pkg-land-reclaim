<script setup lang="ts">
// Which shares keep the copy of a duplicate group, in the user's order: a
// copy in the first listed share stays before any other. Stored on the NAS
// per volume, applied at once; a change starts the duplicate analysis again,
// and Duplicates shows "recalculating" until it ends.
import { computed, ref, watch } from 'vue'
import { Button, CellText, IconButton, Select, SettingsRow, SettingsSection, useToast, type DataGridColumn, type DataGridReorder } from '@synopackageland/ui'
import { FolderOpen, X } from '@lucide/vue'
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import { formatVolume, t } from '@/i18n'
import { selectedVolume, volumeList } from '@/stores/volumes'
import { preferredShares, setPreferredShares } from '@/pages/Duplicates/duplicatesClient'

const toast = useToast()
const preferred = ref<string[]>([])
const loaded = ref(false)
const saving = ref(false)
const adding = ref('')

// Duplicates exist on Btrfs volumes only; the list starts on the page's volume.
const btrfsVolumes = computed(() => volumeList.value.filter(volume => volume.filesystem === 'btrfs'))
const volume = ref('')
watch([btrfsVolumes, selectedVolume], ([list, selected]) => {
  if (list.some(entry => entry.mount.path === volume.value)) return
  volume.value = (list.find(entry => entry.mount.path === selected?.mount.path) ?? list[0])?.mount.path ?? ''
}, { immediate: true })
const volumeChoices = computed(() => btrfsVolumes.value.map(entry => ({ value: entry.mount.path, label: formatVolume(entry.mount.path) })))

let request = 0
watch(volume, async path => {
  const id = ++request
  loaded.value = false
  preferred.value = []
  if (!path) return
  try {
    const answer = await preferredShares(path)
    if (id === request) preferred.value = answer.shares
  } catch {
    if (id === request) toast.danger(t('settings.preferredShares.loadFailed'))
  } finally {
    if (id === request) loaded.value = true
  }
}, { immediate: true })

const rows = computed(() => preferred.value.map((share, index) => ({ id: share, share, order: index + 1 })))
const columns = computed<DataGridColumn[]>(() => [
  { key: 'order', label: '#', width: 48 },
  { key: 'share', label: t('settings.preferredShares.share'), flex: 1, minWidth: 140 },
  { key: 'actions', label: '', width: 56, align: 'right' },
])
const choices = computed(() => [...new Set((btrfsVolumes.value.find(entry => entry.mount.path === volume.value)?.shares ?? []).map(entry => entry.path.path.split('/').at(-1) ?? ''))]
  .filter(name => name && !preferred.value.includes(name))
  .sort()
  .map(name => ({ value: name, label: name })))

async function save(next: string[]) {
  const before = preferred.value
  preferred.value = next
  saving.value = true
  try {
    const answer = await setPreferredShares(volume.value, next)
    if (answer.recalculating) toast.info(t('settings.preferredShares.recalculating'))
  } catch {
    preferred.value = before
    toast.danger(t('settings.preferredShares.saveFailed'))
  } finally {
    saving.value = false
  }
}
function reorder(event: DataGridReorder) {
  void save(event.rows.map(row => String(row.share)))
}
function add() {
  if (!adding.value) return
  void save([...preferred.value, adding.value])
  adding.value = ''
}
</script>

<template>
  <SettingsSection :collapsible="false" :title="t('settings.preferredShares.title')" :description="t('settings.preferredShares.description')">
    <SettingsRow v-if="volumeChoices.length > 1" :label="t('settings.preferredShares.volume')">
      <Select v-model="volume" class="preferred-shares__select" :options="volumeChoices" :aria-label="t('settings.preferredShares.volume')" />
    </SettingsRow>
    <div v-if="loaded" class="preferred-shares" data-testid="preferred-shares">
      <ReclaimGrid
        v-if="rows.length"
        table-id="preferred-shares"
        :fill="false"
        :rows="rows"
        :columns="columns"
        row-key="id"
        draggable
        @reorder="reorder"
      >
        <template #cell-share="{ row }"><CellText :icon="FolderOpen" :text="String(row.share)" /></template>
        <template #cell-actions="{ row }">
          <IconButton size="sm" variant="ghost" :label="t('settings.preferredShares.remove', { share: String(row.share) })" :disabled="saving" @click="save(preferred.filter(name => name !== row.share))">
            <X class="size-4" aria-hidden="true" />
          </IconButton>
        </template>
      </ReclaimGrid>
      <p v-else class="preferred-shares__empty">{{ t('settings.preferredShares.none') }}</p>
    </div>
    <SettingsRow :label="t('settings.preferredShares.addLabel')">
      <div class="preferred-shares__add">
        <Select v-model="adding" class="preferred-shares__select" :options="choices" :placeholder="t('settings.preferredShares.choose')" :aria-label="t('settings.preferredShares.addLabel')" />
        <Button :disabled="!adding || saving" @click="add">{{ t('settings.preferredShares.add') }}</Button>
      </div>
    </SettingsRow>
  </SettingsSection>
</template>

<style scoped>
.preferred-shares {
  margin-bottom: 12px;
}
.preferred-shares__empty {
  margin: 0;
  color: var(--ui-text-secondary);
  font-size: var(--ui-font-size-sm);
}
.preferred-shares__add {
  display: flex;
  gap: 8px;
  align-items: center;
}
.preferred-shares__select {
  width: 200px;
}
</style>
