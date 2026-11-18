/* eslint vue/one-component-per-file: off */
import { mount } from '@vue/test-utils'
import { defineComponent } from 'vue'
import OptionPicker from './OptionPicker.vue'
import type { PickerOption } from './optionList'

const SearchFieldStub = defineComponent({
  props: { modelValue: { type: String, default: '' }, placeholder: { type: String, default: '' }, clearLabel: { type: String, default: '' } },
  emits: ['update:modelValue'],
  template: '<input class="search" :value="modelValue" :placeholder="placeholder" @input="$emit(\'update:modelValue\', $event.target.value)" />',
})

const ButtonStub = defineComponent({
  props: { disabled: Boolean, loading: Boolean, size: { type: String, default: '' }, variant: { type: String, default: '' } },
  emits: ['click'],
  template: '<button :disabled="disabled" @click="$emit(\'click\')"><slot /></button>',
})

const options = (count: number, extra: (index: number) => Partial<PickerOption> = () => ({})): PickerOption[] =>
  Array.from({ length: count }, (_, index) => ({ value: `share${index}`, label: `share${String(index).padStart(3, '0')}`, ...extra(index) }))

const mountPicker = (props: { options: PickerOption[]; modelValue?: string[]; multiple?: boolean; ariaLabel?: string; maxSelected?: number }) => mount(OptionPicker, {
  props: { modelValue: [], ariaLabel: 'Choose', ...props },
  global: { stubs: { SearchField: SearchFieldStub, Button: ButtonStub } },
})

type Picker = ReturnType<typeof mountPicker>
const rows = (wrapper: Picker) => wrapper.findAll('[role="checkbox"], [role="radio"]')
const rowText = (wrapper: Picker) => wrapper.findAll('label').filter(label => label.find('[role="checkbox"], [role="radio"]').exists()).map(label => label.text())

describe('OptionPicker', () => {
  it('shows a short list without a search field', () => {
    const wrapper = mountPicker({ options: options(5) })
    expect(wrapper.find('.search').exists()).toBe(false)
    expect(rows(wrapper)).toHaveLength(5)
  })

  it('offers name search above twenty options and filters as you type', async () => {
    const wrapper = mountPicker({ options: options(25) })
    expect(wrapper.find('.search').exists()).toBe(true)
    expect(rows(wrapper)).toHaveLength(25)

    await wrapper.find('.search').setValue('share02')
    const labels = rowText(wrapper)
    expect(labels).toHaveLength(5)
    expect(labels[0]).toContain('share020')

    await wrapper.find('.search').setValue('nothing')
    expect(rows(wrapper)).toHaveLength(0)
    expect(wrapper.find('.option-picker__empty').exists()).toBe(true)
  })

  it('groups options by volume, collapsed unless the group holds a selection', () => {
    const grouped = [
      { value: 'a', label: 'alpha', group: 'volume1' },
      { value: 'b', label: 'beta', group: 'volume2' },
      { value: 'c', label: 'gamma', group: 'volume2' },
    ]
    const wrapper = mountPicker({ options: options(20).concat(grouped), modelValue: ['b'], multiple: true })
    const heads = wrapper.findAll('button[aria-expanded]')
    expect(heads).toHaveLength(2)
    expect(heads[1].attributes('aria-expanded')).toBe('true')
    expect(heads[0].attributes('aria-expanded')).toBe('false')
  })

  it('selects every shown, unblocked option with select all', async () => {
    const picker = options(21)
    picker[0] = { ...picker[0], disabledReason: 'no space' }
    const wrapper = mountPicker({ options: picker, multiple: true })
    const selectAll = wrapper.findAll('button').find(button => button.text().includes('Select all shown'))
    expect(selectAll?.text()).toContain('20')
    await selectAll?.trigger('click')
    const emitted = wrapper.emitted('update:modelValue')?.at(-1)?.[0] as string[]
    expect(emitted).toHaveLength(20)
    expect(emitted).not.toContain('share000')
  })

  it('pages a list above two hundred options fifty rows at a time', async () => {
    const wrapper = mountPicker({ options: options(201) })
    expect(rows(wrapper)).toHaveLength(50)
    expect(wrapper.find('nav [aria-current="page"]').text()).toBe('1')
    expect(wrapper.find('nav button[aria-label="Go to page 5"]').exists()).toBe(true)

    await wrapper.find('nav button[aria-label="Next"]').trigger('click')
    expect(rowText(wrapper)[0]).toContain('share050')
  })

  it('replaces the choice in a single-select list and skips blocked rows', async () => {
    const single = [
      { value: 'a', label: 'alpha' },
      { value: 'b', label: 'beta', disabledReason: 'no space' },
    ]
    const wrapper = mountPicker({ options: single, modelValue: ['a'] })
    await rows(wrapper)[1].trigger('click')
    expect(wrapper.emitted('update:modelValue')).toBeUndefined()

    await rows(wrapper)[0].trigger('click')
    expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual(['a'])
  })

  it('clears the selection', async () => {
    const wrapper = mountPicker({ options: options(21), modelValue: ['share001'], multiple: true })
    const clear = wrapper.findAll('button').find(button => button.text().includes('Clear selection'))
    await clear?.trigger('click')
    expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual([])
  })

  it('never selects more than the given maximum', async () => {
    const wrapper = mountPicker({ options: options(30), multiple: true, maxSelected: 3 })
    const selectAll = wrapper.findAll('button').find(button => button.text().includes('Select all shown'))
    await selectAll?.trigger('click')
    expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual(['share0', 'share1', 'share2'])
  })

  it('locks unselected rows once the maximum is reached', () => {
    const wrapper = mountPicker({ options: options(25), multiple: true, maxSelected: 2, modelValue: ['share0', 'share1'] })
    const boxes = rows(wrapper)
    expect(boxes[0].attributes('disabled')).toBeUndefined()
    expect(boxes[2].attributes('disabled')).toBeDefined()
    expect(wrapper.find('.option-picker__count').text()).toContain('2 of 2 selected')
  })
})
