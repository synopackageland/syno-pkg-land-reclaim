import type { InjectionKey } from 'vue'
import type { AppWindowVisibility } from '../usage/appVisibility'

/** The DSM AppWindow the bundle mounts in (the window `synopkgland build` generates). */
export interface DsmAppWindow extends AppWindowVisibility {
  toFront: () => unknown
}

/** Brings the owning DSM window to the front; not provided outside a DSM window. */
export const DSM_WINDOW_FRONT: InjectionKey<() => void> = Symbol('dsmWindowFront')
