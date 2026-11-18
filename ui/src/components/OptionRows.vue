<script setup lang="ts">
// The rows of one option list (OptionPicker): a checkbox per option for
// many-value lists, one radio group for one-value lists, each with its note
// and refusal reason.
import { Checkbox, RadioGroup } from '@synopackageland/ui'
import type { RadioOption } from '@synopackageland/ui'
import { joinParts, locale } from '@/i18n'
import type { PickerOption } from './optionList'

const props = defineProps<{
  options: PickerOption[]
  multiple: boolean
  chosen: (value: string) => boolean
  disabled: (option: PickerOption) => boolean
}>()
const emit = defineEmits<{ toggle: [option: PickerOption] }>()

/** The note under a row: its size or free space, then why it cannot be chosen. */
function note(option: PickerOption) {
  return joinParts([option.meta, option.disabledReason].filter((part): part is string => Boolean(part)), locale) || undefined
}

const radios = (): RadioOption[] => props.options.map(option => ({
  value: option.value,
  label: option.label,
  description: note(option),
  disabled: props.disabled(option),
}))

function pick(value: string) {
  const option = props.options.find(candidate => candidate.value === value)
  if (option) emit('toggle', option)
}
</script>

<template>
  <div class="option-rows">
    <template v-if="multiple">
      <Checkbox
        v-for="option in options" :key="option.value" :model-value="chosen(option.value)" :label="option.label"
        :description="note(option)" :disabled="disabled(option)" @update:model-value="emit('toggle', option)"
      />
    </template>
    <RadioGroup
      v-else :model-value="options.find(option => chosen(option.value))?.value" :options="radios()"
      @update:model-value="pick"
    />
  </div>
</template>

<style scoped>
.option-rows { display: grid; gap: 8px; padding: 8px 12px; }
</style>
