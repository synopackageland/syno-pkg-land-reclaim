/* eslint vue/one-component-per-file: off */
import { mount } from '@vue/test-utils'
import { defineComponent } from 'vue'
import VolumePicker from './VolumePicker.vue'
import type { PickerOption } from './optionList'

const SectionTabsStub = defineComponent({
  props: { modelValue: { type: String, default: '' }, items: { type: Array, default: () => [] }, ariaLabel: { type: String, default: '' } },
  emits: ['update:modelValue'],
  template: '<div class="stub-tabs"><button v-for="item in items" :key="item.value" @click="$emit(\'update:modelValue\', item.value)">{{ item.label }}</button></div>',
})

const SelectStub = defineComponent({
  props: { modelValue: { type: String, default: '' }, options: { type: Array, default: () => [] }, ariaLabel: { type: String, default: '' } },
  emits: ['update:modelValue'],
  template: '<select class="stub-select"><option v-for="option in options" :key="option.value" :value="option.value">{{ option.label }}</option></select>',
})

const OptionPickerStub = defineComponent({
  props: { modelValue: { type: Array, default: () => [] }, options: { type: Array, default: () => [] }, searchFrom: { type: Number, default: 20 }, ariaLabel: { type: String, default: '' } },
  emits: ['update:modelValue'],
  template: '<div class="stub-picker">{{ options.length }}</div>',
})

const PageMetaStub = defineComponent({
  props: { menu: { type: Array, default: () => [] } },
  emits: ['select'],
  template: '<div class="stub-meta"><slot /></div>',
})

const volumes = (count: number): PickerOption[] => Array.from({ length: count }, (_, index) => ({ value: `/volume${index + 1}`, label: `volume${index + 1}` }))

const mountPicker = (count: number, modelValue = '/volume1') => mount(VolumePicker, {
  props: { modelValue, volumes: volumes(count), ariaLabel: 'Volume' },
  global: { stubs: { SectionTabs: SectionTabsStub, Select: SelectStub, OptionPicker: OptionPickerStub } },
})

describe('VolumePicker', () => {
  it('shows tabs for up to four volumes', () => {
    const wrapper = mountPicker(4)
    expect(wrapper.find('.stub-tabs').exists()).toBe(true)
    expect(wrapper.find('.stub-select').exists()).toBe(false)
    expect(wrapper.find('.stub-picker').exists()).toBe(false)
  })

  // ui 0.9.3 names the tab strip "Sections" unless told otherwise.
  it('names the tab strip with its label', () => {
    expect(mountPicker(2).findComponent(SectionTabsStub).props('ariaLabel')).toBe('Volume')
  })

  it('uses one dropdown for five to twenty volumes', () => {
    const wrapper = mountPicker(8)
    expect(wrapper.find('.stub-select').exists()).toBe(true)
    expect(wrapper.find('.stub-tabs').exists()).toBe(false)
  })

  it('uses the searchable picker above twenty volumes', () => {
    const wrapper = mountPicker(21)
    expect(wrapper.find('.stub-picker').exists()).toBe(true)
    expect(wrapper.find('.stub-picker').text()).toBe('21')
    expect(wrapper.find('.stub-select').exists()).toBe(false)
  })

  it('emits the chosen volume', async () => {
    const wrapper = mountPicker(3)
    await wrapper.findAll('.stub-tabs button')[2].trigger('click')
    expect(wrapper.emitted('update:modelValue')?.at(-1)).toEqual(['/volume3'])
  })

  describe('in the page bar', () => {
    const mountBar = (count: number, modelValue = '/volume1') => mount(VolumePicker, {
      props: { modelValue, volumes: volumes(count), ariaLabel: 'Volume', placement: 'bar' as const },
      global: { stubs: { SectionTabs: SectionTabsStub, Select: SelectStub, OptionPicker: OptionPickerStub, PageMeta: PageMetaStub } },
    })

    it('shows nothing for one volume', () => {
      const wrapper = mountBar(1)
      expect(wrapper.find('.stub-meta').exists()).toBe(false)
      expect(wrapper.find('.stub-select').exists()).toBe(false)
    })

    it('uses one compact dropdown in the page meta for two to twenty volumes', () => {
      for (const count of [2, 4, 20]) {
        const wrapper = mountBar(count)
        expect(wrapper.find('.stub-meta .stub-select').exists()).toBe(true)
        expect(wrapper.find('.stub-tabs').exists()).toBe(false)
        expect(wrapper.find('.volume-picker__select').exists()).toBe(true)
      }
    })

    it('uses the searchable picker above twenty volumes', () => {
      const wrapper = mountBar(21)
      expect(wrapper.find('.stub-meta .stub-picker').exists()).toBe(true)
      expect(wrapper.find('.stub-select').exists()).toBe(false)
    })

    it('offers the volumes in the phone menu, the current one checked, and emits a choice', async () => {
      const wrapper = mountBar(3, '/volume2')
      const [entry] = wrapper.findComponent(PageMetaStub).props('menu') as { label: string, children: { key: string, label: string, checked: boolean }[] }[]
      expect(entry!.label).toBe('Volume')
      expect(entry!.children.map(child => [child.label, child.checked])).toEqual([['volume1', false], ['volume2', true], ['volume3', false]])
      wrapper.findComponent(PageMetaStub).vm.$emit('select', entry!.children[2]!.key)
      wrapper.findComponent(PageMetaStub).vm.$emit('select', 'other')
      expect(wrapper.emitted('update:modelValue')).toEqual([['/volume3']])
    })
  })
})
