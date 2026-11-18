<script setup lang="ts">
import { AppActionBar, Button, IconButton } from '@synopackageland/ui'
import { X } from '@lucide/vue'

const props = defineProps<{
  countLabel?: string
  estimateText: string
  estimateLead?: string
  summaryLead?: string
  summaryAfter?: string
  summaryDetails?: string
  details?: string[]
  clearLabel: string
  reviewLabel?: string
  emphasis?: boolean
  /** One row for a short window: the summary stays on one line and Clear is an icon. */
  compact?: boolean
  /** The whole summary, as the tooltip of a compact one. */
  summaryTitle?: string
}>()

const emit = defineEmits<{ clear: []; review: [] }>()
</script>

<template>
  <AppActionBar
    :class="props.emphasis
      ? 'explore-basket-bar shrink-0 rounded-t-[var(--ui-radius-lg)] border-x border-t text-[var(--ui-surface)] shadow-[var(--ui-shadow-menu)]'
      : undefined"
  >
    <template #leading>
      <div
        class="flex min-w-0 items-baseline gap-y-1 px-1 py-1"
        :class="[props.summaryLead ? 'gap-x-0' : 'gap-x-4', props.compact ? 'whitespace-nowrap' : 'flex-wrap']"
        :title="props.compact ? props.summaryTitle : undefined"
      >
        <template v-if="props.summaryLead">
          <span
            class="text-[13px]"
            :style="props.emphasis ? { color: 'color-mix(in srgb, var(--ui-surface) 72%, transparent)' } : undefined"
          >{{ props.summaryLead }}&nbsp;</span>
          <strong class="text-[19px] font-semibold text-[var(--ui-primary)]">{{ props.estimateText }}</strong>
          <span
            v-if="props.summaryAfter"
            class="text-[13px]"
            :style="props.emphasis ? { color: 'color-mix(in srgb, var(--ui-surface) 72%, transparent)' } : undefined"
          >{{ props.summaryAfter }}</span>
          <span
            v-if="props.summaryDetails"
            class="text-[12px]"
            :style="props.emphasis ? { color: 'color-mix(in srgb, var(--ui-surface) 72%, transparent)' } : undefined"
          >{{ props.summaryDetails }}</span>
        </template>
        <template v-else>
          <span
            v-if="props.countLabel"
            :class="props.emphasis ? 'shrink-0 text-[13px]' : 'shrink-0 text-[13px] font-medium'"
            :style="props.emphasis ? { color: 'color-mix(in srgb, var(--ui-surface) 72%, transparent)' } : undefined"
          >
            {{ props.countLabel }}
          </span>
          <span class="text-[13px]" :style="props.emphasis ? { color: 'var(--ui-surface)' } : undefined">
            <span v-if="props.estimateLead">{{ props.estimateLead }} </span>
            <strong v-if="props.emphasis" class="ml-1 text-[19px] font-semibold text-[var(--ui-primary)]">{{ props.estimateText }}</strong>
            <span v-else class="text-[var(--ui-text-muted)]">{{ props.estimateText }}</span>
          </span>
          <span
            v-for="detail in props.details ?? []"
            :key="detail"
            class="text-[12px]"
            :style="props.emphasis ? { color: 'color-mix(in srgb, var(--ui-surface) 72%, transparent)' } : undefined"
          >
            {{ detail }}
          </span>
        </template>
      </div>
    </template>
    <div
      v-if="$slots.meter"
      class="mr-2 min-w-0"
      :style="props.emphasis ? { color: 'var(--ui-surface)' } : undefined"
    ><slot name="meter" /></div>
    <IconButton
      v-if="props.compact"
      :label="props.clearLabel"
      :title="props.clearLabel"
      :class="props.emphasis ? 'hover:bg-[color-mix(in_srgb,var(--ui-surface)_12%,transparent)]' : undefined"
      :style="props.emphasis ? { color: 'var(--ui-surface)' } : undefined"
      @click="emit('clear')"
    >
      <X class="size-4" aria-hidden="true" />
    </IconButton>
    <Button
      v-else
      variant="ghost"
      :size="props.emphasis ? undefined : 'sm'"
      :class="props.emphasis ? 'hover:bg-[color-mix(in_srgb,var(--ui-surface)_12%,transparent)]' : undefined"
      :style="props.emphasis ? { color: 'var(--ui-surface)' } : undefined"
      @click="emit('clear')"
    >
      {{ props.clearLabel }}
    </Button>
    <Button v-if="props.reviewLabel" variant="primary" @click="emit('review')">
      {{ props.reviewLabel }}
    </Button>
  </AppActionBar>
</template>

<style scoped>
.explore-basket-bar {
  background: var(--ui-text);
  border-color: var(--ui-border-strong);
}
</style>
