import { createUiApp } from '@/createUiApp'
import { setBackend } from '@/api'
import '@/styles'
import RootfsCleanupPreview from './RootfsCleanupPreview.vue'
import { rootfsCleanupFixtureBackend } from './cleanupFixtureBackend'

setBackend(rootfsCleanupFixtureBackend())
createUiApp(RootfsCleanupPreview).mount('#rootfs-cleanup-preview')
