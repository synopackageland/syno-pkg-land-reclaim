import { createUiApp } from '@/createUiApp'
import { setBackend } from '@/api'
import type { Backend } from '@/api/methods'
import './styles'
import RootfsApplyFixtureApp from '@/pages/RootfsApply/RootfsApplyFixtureApp.vue'
import { rootfsApplyFixtureBackend } from '@/pages/RootfsApply/rootfsApplyFixtureBackend'

setBackend(rootfsApplyFixtureBackend as unknown as Backend)
createUiApp(RootfsApplyFixtureApp).mount('#rootfs-fixture')
