import { createUiApp } from '@/createUiApp'
import './styles'
import App from './App.vue'
import { SHELL_CONTEXT } from './context'
import { configureAuthorization } from './stores/authorization'
import { fixtureAuthorizationClient } from './api/authorizationFixtures'

// Outside DSM there is no package CGI, so the dev server enters with a ready
// fixture. The gate's other states are shown on authorization.html.
configureAuthorization({ client: fixtureAuthorizationClient({ state: 'ready', reason: null }) })
// Nor is there a DSM session to name LUNs and VMM disks; the fixture inventory stands in.
if (import.meta.env.DEV) {
  void Promise.all([import('./dsm/snapshotInventoryFixture'), import('./dsm/snapshotInventory')])
    .then(([{ fixtureInventorySource }, { setDsmInventorySource }]) => setDsmInventorySource(fixtureInventorySource))
  // The fixture duplicates' Archive share has no Recycle Bin, as a share can.
  void import('./dsm/recycleBins').then(({ setRecycleBinSource }) => setRecycleBinSource(() => Promise.resolve(new Map([
    ['Archive', { enabled: false, adminOnly: false }],
  ]))))
}
// The dev page owns the browser window like a Portal launch; browser/dsm-layout.html
// stands in a DSM window (it defines SYNO).
createUiApp(App).provide(SHELL_CONTEXT, { fullPage: !window.SYNO, displayName: 'Syno Reclaim' }).mount('#synoreclaim')
