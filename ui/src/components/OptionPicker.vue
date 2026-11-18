<script setup lang="ts">
// A list of choices that scales (design review 2026-10-01 §1): short lists
// stay plain, longer ones get name search, collapsible volume groups, an
// honest "select all shown" and 50-row pages above 200 options.
import { computed, ref, watch } from 'vue'
import { Button, ExpandableItem, Pagination, SearchField } from '@synopackageland/ui'
import type { PaginationLabels } from '@synopackageland/ui'
import { t } from '@/i18n'
import OptionRows from './OptionRows.vue'
import {
  filterOptions,
  groupOptions,
  needsSearch,
  PAGE_SIZE,
  pageCount,
  pageSlice,
  selectableValues,
  toggledValues,
  type OptionGroup,
  type PickerOption,
} from './optionList'

const props = withDefaults(defineProps<{
  modelValue: string[]
  options: PickerOption[]
  /** Many values (checkboxes) or one (radios). */
  multiple?: boolean
  ariaLabel: string
  /** Above this many options, offer name search (default 20). */
  searchFrom?: number
  /** A multi-choice list takes at most this many values; unselected rows lock at the cap. */
  maxSelected?: number
  emptyText?: string
}>(), { multiple: false, searchFrom: 20 })

const emit = defineEmits<{ 'update:modelValue': [value: string[]] }>()

const query = ref('')
const page = ref(0)
const openGroups = ref(new Set<string>())
const closedGroups = ref(new Set<string>())

watch(query, () => { page.value = 0 })
watch(() => props.options, () => { page.value = 0 })

const searchable = computed(() => needsSearch(props.options.length, props.searchFrom))
const filtered = computed(() => filterOptions(props.options, query.value))
const pageTotal = computed(() => pageCount(filtered.value.length))
const paged = computed(() => pageTotal.value > 1)
const visible = computed(() => (paged.value ? pageSlice(filtered.value, page.value) : filtered.value))
const groups = computed<OptionGroup[]>(() => groupOptions(visible.value))
const shownSelectable = computed(() => selectableValues(filtered.value))

const pageLabels = computed<Partial<PaginationLabels>>(() => ({
  nav: () => props.ariaLabel,
  previous: t('common.picker.previous'),
  next: t('common.picker.next'),
  page: n => t('common.picker.page', { count: n, total: pageTotal.value }),
  goToPage: n => t('common.picker.goToPage', { count: n }),
}))

function chosen(value: string) {
  return props.modelValue.includes(value)
}

/** True when the list is full and this row is not one of the chosen ones. */
function rowDisabled(option: PickerOption) {
  if (option.disabledReason) return true
  return props.multiple && props.maxSelected != null && !chosen(option.value) && props.modelValue.length >= props.maxSelected
}

function groupHasSelection(group: OptionGroup) {
  return group.options.some(option => chosen(option.value))
}

function groupIsOpen(group: OptionGroup) {
  const key = group.label ?? ''
  if (openGroups.value.has(key)) return true
  if (closedGroups.value.has(key)) return false
  return groupHasSelection(group)
}

function setGroupOpen(group: OptionGroup, open: boolean) {
  const key = group.label ?? ''
  if (!open) {
    openGroups.value.delete(key)
    closedGroups.value.add(key)
  } else {
    closedGroups.value.delete(key)
    openGroups.value.add(key)
  }
}

function toggle(option: PickerOption) {
  if (rowDisabled(option)) return
  emit('update:modelValue', toggledValues(props.modelValue, option.value, props.multiple))
}

function selectAllShown() {
  const room = props.maxSelected == null ? Infinity : Math.max(0, props.maxSelected - props.modelValue.length)
  const added = shownSelectable.value.filter(value => !props.modelValue.includes(value)).slice(0, room)
  emit('update:modelValue', [...props.modelValue, ...added])
}

function clearSelection() {
  emit('update:modelValue', [])
}
</script>

<template>
  <div class="option-picker">
    <template v-if="searchable">
      <SearchField v-model="query" :placeholder="t('common.picker.search')" :clear-label="t('common.picker.clearSearch')" />
      <div class="option-picker__bar">
        <span v-if="multiple" class="option-picker__count" role="status">
          {{ maxSelected == null ? t('common.picker.selectedCount', { count: modelValue.length }) : t('common.picker.selectedCountMax', { count: modelValue.length, max: maxSelected }) }}
        </span>
        <div v-if="multiple" class="option-picker__bulk">
          <Button size="sm" variant="ghost" :disabled="!shownSelectable.length" @click="selectAllShown">
            {{ t('common.picker.selectAll', { count: shownSelectable.length }) }}
          </Button>
          <Button size="sm" variant="ghost" :disabled="!modelValue.length" @click="clearSelection">
            {{ t('common.picker.clear') }}
          </Button>
        </div>
      </div>
      <p v-if="!filtered.length" class="option-picker__empty" role="status">
        {{ emptyText ?? t('common.picker.noMatches') }}
      </p>
      <div v-else class="option-picker__groups" role="group" :aria-label="ariaLabel">
        <template v-for="group in groups" :key="group.label ?? ''">
          <ExpandableItem
            v-if="group.label" :title="group.label" :expanded="groupIsOpen(group)" @update:expanded="setGroupOpen(group, $event)"
          >
            <template #status><span class="option-picker__group-count">{{ group.options.length }}</span></template>
            <template #detail>
              <div>
                <OptionRows :options="group.options" :multiple="multiple" :chosen="chosen" :disabled="rowDisabled" @toggle="toggle" />
              </div>
            </template>
          </ExpandableItem>
          <div v-else class="option-picker__list">
            <OptionRows :options="group.options" :multiple="multiple" :chosen="chosen" :disabled="rowDisabled" @toggle="toggle" />
          </div>
        </template>
      </div>
      <Pagination
        v-if="paged" :model-value="page + 1" :total="filtered.length" :page-size="PAGE_SIZE" :show-total="false" :page-shift-unit="1"
        :has-first-btn="false" :has-last-btn="false" :items-per-page="false" :refresh="false" :labels="pageLabels"
        @update:model-value="page = $event - 1"
      />
    </template>

    <div v-else class="option-picker__list" role="group" :aria-label="ariaLabel">
      <OptionRows :options="options" :multiple="multiple" :chosen="chosen" :disabled="rowDisabled" @toggle="toggle" />
    </div>
  </div>
</template>

<style scoped>
.option-picker { display: grid; gap: 8px; }
.option-picker__bar { display: flex; align-items: center; justify-content: space-between; gap: 10px; color: var(--ui-text-muted); font-size: var(--ui-font-size-sm); }
.option-picker__bulk { display: flex; gap: 4px; }
.option-picker__groups { display: grid; max-height: 260px; overflow: auto; border: 1px solid var(--ui-border); border-radius: var(--ui-radius-md); background: var(--ui-surface); }
.option-picker__group-count { color: var(--ui-text-muted); }
.option-picker__empty { color: var(--ui-text-muted); font-size: var(--ui-font-size-sm); }
</style>
