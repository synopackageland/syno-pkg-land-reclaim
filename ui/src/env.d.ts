/// <reference types="vite/client" />
import type { MountedApp, PackageWindowContext } from './dsmBundle'

// What the app reads from the DSM desktop it runs in (absent outside DSM).
declare global {
  interface Window {
    SYNO?: { SDS?: { Session?: { lang?: string; SynoToken?: string } } }
    _S?: (key: string) => string | undefined
    /** The apps the synopkgland build's DSM window mounts, by package. */
    DsmPackageApps?: { SynoReclaim?: { mount: (element: Element, ctx: PackageWindowContext) => MountedApp } }
  }
}
