<script setup lang="ts">
import { ref, watch } from 'vue'
import { Button, Checkbox, Dialog } from '@synopackageland/ui'

const open = defineModel<boolean>('open', { required: true })

const props = withDefaults(defineProps<{
  title: string
  description?: string
  checkLabel: string
  confirmLabel: string
  cancelLabel: string
  closeLabel: string
  disabled?: boolean
  loading?: boolean
  showConfirm?: boolean
}>(), {
  disabled: false,
  loading: false,
  showConfirm: true,
})

const emit = defineEmits<{ confirm: [] }>()
const checked = ref(false)

watch(open, value => {
  if (!value) checked.value = false
})

function confirm() {
  if (checked.value && !props.disabled && !props.loading) emit('confirm')
}
</script>

<!--
  With the confirm button shown, the footer holds the checkbox, Cancel and the
  confirm button, and the footer slot is not rendered: put any other button of
  that state in the actions slot, which comes before Cancel.
-->
<template>
  <Dialog v-model:open="open" :title="title" :description="description" v-bind="description ? {} : { 'aria-describedby': undefined }" :close-label="closeLabel">
    <slot />
    <template #footer>
      <div v-if="showConfirm" class="confirm-dialog__footer">
        <Checkbox v-model="checked" :label="checkLabel" :disabled="disabled || loading" />
        <div class="confirm-dialog__actions">
          <slot name="actions" />
          <Button variant="secondary" :disabled="loading" @click="open = false">{{ cancelLabel }}</Button>
          <Button variant="danger" :disabled="disabled || !checked" :loading="loading" @click="confirm">
            {{ confirmLabel }}
          </Button>
        </div>
      </div>
      <slot v-else name="footer" />
    </template>
  </Dialog>
</template>

<style scoped>
.confirm-dialog__footer {
  display: grid;
  gap: 12px;
  width: 100%;
}

.confirm-dialog__actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
</style>
