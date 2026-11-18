<script setup lang="ts">
// Cancelling already asks for confirmation, so it is outlined, not a solid danger button.
import { Button } from '@synopackageland/ui'
</script>

<template>
  <Button variant="secondary" class="cancel-button"><slot /></Button>
</template>

<style scoped>
.cancel-button { border-color: color-mix(in srgb, var(--ui-danger) 40%, transparent); background: transparent; color: var(--ui-danger-text); }
.cancel-button:hover:not(:disabled) { background: var(--ui-danger-soft); }
</style>
