<script setup lang="ts">
// A loaded NAS can keep the approval's root task queued or running for
// minutes; after 30 s tell the administrator it is still going.
import {computed} from 'vue'
import {Callout} from '@synopackageland/ui'
import {formatDuration,joinSentences,locale,t} from '@/i18n'
import {authorization} from '@/stores/authorization'

const SHOW_AFTER_MS = 30_000
const progress = computed(() => (authorization.progress && authorization.progress.elapsedMs >= SHOW_AFTER_MS ? authorization.progress : null))
</script>

<template>
  <div v-if="progress" class="approval-wait" role="status" aria-live="polite">
    <Callout :dismissible="false" tone="info" :title="t('approvalWait.title')">
      {{ joinSentences([t(progress.state === 'queued' ? 'approvalWait.queued' : 'approvalWait.running'), t('approvalWait.elapsed', { duration: formatDuration(progress.elapsedMs / 1000) })], locale) }}
    </Callout>
  </div>
</template>

<style scoped>
.approval-wait { position: absolute; left: 50%; bottom: 24px; transform: translateX(-50%); width: calc(100% - 48px); max-width: 28rem; z-index: 1; }
</style>
