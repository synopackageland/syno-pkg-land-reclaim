// The Application Portal access the DSM mount installs; Settings shows the
// SDK's portal panel only while one is installed (never outside DSM).
import { shallowRef } from 'vue'
import type { PortalAccess } from '@synopackageland/sdk'

export const portal = shallowRef<PortalAccess>()

export function configurePortal(access: PortalAccess | undefined) {
  portal.value = access
}
