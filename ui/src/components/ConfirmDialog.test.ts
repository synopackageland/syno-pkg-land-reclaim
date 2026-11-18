/* eslint vue/one-component-per-file: off */
import { defineComponent } from 'vue'
import { mount } from '@vue/test-utils'
import ConfirmDialog from './ConfirmDialog.vue'

const DialogStub = defineComponent({
  props: { open: Boolean },
  emits: ['update:open'],
  template: '<div><slot /><slot name="footer" /></div>',
})

const CheckboxStub = defineComponent({
  props: { modelValue: Boolean, label: { type: String, default: '' }, disabled: Boolean },
  emits: ['update:modelValue'],
  template: '<label><input type="checkbox" :checked="modelValue" :disabled="disabled" @change="$emit(\'update:modelValue\', $event.target.checked)" />{{ label }}</label>',
})

const ButtonStub = defineComponent({
  props: { disabled: Boolean, loading: Boolean, variant: { type: String, default: 'secondary' } },
  emits: ['click'],
  template: '<button :disabled="disabled || loading" @click="$emit(\'click\')"><slot /></button>',
})

describe('ConfirmDialog', () => {
  it('requires the permanent-delete checkbox before emitting confirm', async () => {
    const wrapper = mount(ConfirmDialog, {
      props: {
        open: true,
        title: 'Review cleanup',
        checkLabel: 'I understand',
        confirmLabel: 'Delete',
        cancelLabel: 'Cancel',
        closeLabel: 'Close',
      },
      global: { stubs: { Dialog: DialogStub, Checkbox: CheckboxStub, Button: ButtonStub } },
    })

    const buttons = wrapper.findAll('button')
    expect(buttons[1].element.disabled).toBe(true)
    await wrapper.find('input[type="checkbox"]').setValue(true)
    expect(wrapper.findAll('button')[1].element.disabled).toBe(false)
    await wrapper.findAll('button')[1].trigger('click')
    expect(wrapper.emitted('confirm')).toHaveLength(1)
  })

  it('renders the actions slot beside the confirm button, and the footer slot only without one', () => {
    const mountWith = (showConfirm: boolean) => mount(ConfirmDialog, {
      props: { open: true, title: 'Review cleanup', checkLabel: 'I understand', confirmLabel: 'Delete', cancelLabel: 'Cancel', closeLabel: 'Close', showConfirm },
      slots: { actions: '<button data-test="extra">Recheck</button>', footer: '<button data-test="footer">Close</button>' },
      global: { stubs: { Dialog: DialogStub, Checkbox: CheckboxStub, Button: ButtonStub } },
    })
    const withConfirm = mountWith(true)
    expect(withConfirm.find('.confirm-dialog__actions [data-test="extra"]').exists()).toBe(true)
    expect(withConfirm.find('[data-test="footer"]').exists()).toBe(false)
    const without = mountWith(false)
    expect(without.find('[data-test="extra"]').exists()).toBe(false)
    expect(without.find('[data-test="footer"]').exists()).toBe(true)
  })
})
