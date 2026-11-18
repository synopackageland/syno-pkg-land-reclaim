<script setup lang="ts">
// One volume selector for every page (design review 2026-10-01 §1):
// 1–4 volumes as tabs, 5–20 as one dropdown, more as a searchable select.
// In the page bar (placement="bar") it is always one compact dropdown, a
// searchable select above twenty volumes, and on a phone an entry of the
// bar's menu; one volume offers no choice, so the bar shows none.
import { computed } from 'vue'
import { Select, SectionTabs, type DropdownMenuEntry } from '@synopackageland/ui'
import { PageMeta } from '@synopackageland/sdk/vue'
import OptionPicker from '@/components/OptionPicker.vue'
import type { PickerOption } from '@/components/optionList'
import { barVolumeLayout, volumeLayout, VOLUME_DROPDOWN_MAX, type ChoiceLayout } from '@/components/optionList'

const props = defineProps<{
  modelValue: string
  volumes: PickerOption[]
  ariaLabel: string
  placement?: 'bar'
}>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const inBar = computed(() => props.placement === 'bar')
const layout = computed<ChoiceLayout | null>(() => inBar.value ? barVolumeLayout(props.volumes.length) : volumeLayout(props.volumes.length))
const tabs = computed(() => props.volumes.map(volume => ({ value: volume.value, label: volume.label, disabled: !!volume.disabledReason })))
const dropdown = computed(() => props.volumes.map(volume => ({ value: volume.value, label: volume.label, disabled: !!volume.disabledReason })))

const MENU_PREFIX = 'volume:'
/** The phone bar's "…" takes the volumes as one submenu, the current one checked. */
const menu = computed<DropdownMenuEntry[]>(() => [{
  key: 'volume',
  label: props.ariaLabel,
  children: props.volumes.map(volume => ({
    key: `${MENU_PREFIX}${volume.value}`,
    label: volume.label,
    checked: volume.value === props.modelValue,
    disabled: !!volume.disabledReason,
  })),
}])

function selectFromMenu(key: string) {
  if (key.startsWith(MENU_PREFIX)) emit('update:modelValue', key.slice(MENU_PREFIX.length))
}
</script>

<template>
  <PageMeta v-if="inBar && layout" :menu="menu" @select="selectFromMenu">
    <div class="volume-picker volume-picker--bar" :class="`volume-picker--${layout}`">
      <Select
        v-if="layout === 'dropdown'"
        class="volume-picker__select"
        :model-value="modelValue"
        :options="dropdown"
        :aria-label="ariaLabel"
        @update:model-value="emit('update:modelValue', $event)"
      />
      <OptionPicker
        v-else
        v-bind="{ ariaLabel }"
        :model-value="[modelValue]"
        :options="volumes"
        :search-from="VOLUME_DROPDOWN_MAX"
        @update:model-value="emit('update:modelValue', $event[0] ?? '')"
      />
    </div>
  </PageMeta>
  <div v-else-if="!inBar" class="volume-picker" :class="`volume-picker--${layout}`">
    <SectionTabs
      v-if="layout === 'tabs'"
      :aria-label="ariaLabel"
      :model-value="modelValue"
      :items="tabs"
      @update:model-value="emit('update:modelValue', $event)"
    />
    <Select
      v-else-if="layout === 'dropdown'"
      :model-value="modelValue"
      :options="dropdown"
      :aria-label="ariaLabel"
      @update:model-value="emit('update:modelValue', $event)"
    />
    <OptionPicker
      v-else
      v-bind="{ ariaLabel }"
      :model-value="[modelValue]"
      :options="volumes"
      :search-from="VOLUME_DROPDOWN_MAX"
      @update:model-value="emit('update:modelValue', $event[0] ?? '')"
    />
  </div>
</template>

<style scoped>
.volume-picker {
  min-width: 0;
}
/* In the page bar the picker is a fixed, compact control beside the info button. */
.volume-picker--bar {
  display: inline-flex;
  align-items: center;
}
.volume-picker__select {
  width: 128px;
}
</style>
