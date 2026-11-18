// The NAS's Btrfs volumes (volumes.list) and the volume the pages show.
import { computed, reactive, watch } from 'vue'
import { api, ApiError } from '@/api'
import type { Volume, VolumesDocument } from '@/types'
import { readSession, writeSession } from './storage'

const SELECTED_KEY = 'synoreclaim.volume'

export const volumes = reactive({
  loading: false,
  document: null as VolumesDocument | null,
  error: null as ApiError | null,
  /** Mount path of the selected volume. */
  selected: readSession<string | null>(SELECTED_KEY, null),
})

watch(() => volumes.selected, value => writeSession(SELECTED_KEY, value))

export const volumeList = computed<Volume[]>(() => volumes.document?.volumes ?? [])
export const selectedVolume = computed(
  () => volumeList.value.find(v => v.mount.path === volumes.selected) ?? volumeList.value[0] ?? null,
)

/** Mount paths of the volumes skipped because their file system is unsupported (neither Btrfs nor ext4). */
export const unsupportedVolumes = computed<string[]>(() =>
  (volumes.document?.excluded ?? []).filter(e => e.code === 'unsupported_filesystem').map(e => e.path),
)

export async function loadVolumes() {
  volumes.loading = true
  try {
    volumes.document = await api('volumes.list', {})
    volumes.error = null
  } catch (error) {
    volumes.error = error instanceof ApiError ? error : new ApiError('unknown')
  } finally {
    volumes.loading = false
  }
}
