// Page selection (no router: the URL belongs to DSM).
import { reactive, watch } from 'vue'
import type { Path } from '@/types'
import { readSession, writeSession } from './storage'

const PAGE_KEY = 'synoreclaim.page'

export const restoredPage = () => readSession<string | null>(PAGE_KEY, null)

export const nav = reactive({
  page: restoredPage() ?? 'overview',
  followedJobId: null as string | null,
  newScanOpen: false,
  /** A History detail requested the Overview's System partition view. */
  rootfsOpenRequested: false,
  rootfsResultJobId: null as string | null,
  /** New scan asked the Overview to check the System partition again. */
  rootfsCheckRequested: false,
  /** A page asked the Packages page to expand one package; taken once. */
  packageRequest: null as { service: string; path: string } | null,
  /** A page asked Explore to open a path; taken once. */
  exploreRequest: null as Path | null,
})

watch(() => nav.page, page => writeSession(PAGE_KEY, page), { flush: 'sync' })

export function navigate(page: string) {
  nav.page = page
}

export function openNewScan() {
  nav.newScanOpen = true
}

/** Return to Overview and select its System partition card. */
export function openRootfsView(jobId?: string) {
  nav.rootfsResultJobId = jobId ?? null
  nav.rootfsOpenRequested = true
  nav.page = 'overview'
}

/** Return to Overview, select its System partition card and check it again. */
export function checkRootfs() {
  nav.rootfsCheckRequested = true
  nav.page = 'overview'
}

/** Open a backend job by id and kind after any app/session restart. */
export function openJob(job: { job_id: string; kind: string }) {
  nav.followedJobId = job.job_id
  navigate(job.kind === 'rootfs-apply' ? 'rootfsApply' : job.kind === 'cleanup-apply' ? 'cleanupApply' : 'scanning')
}

/** Show the Packages page with one package expanded (a service row of Explore). */
export function openPackage(service: string, path: string) {
  nav.packageRequest = { service, path }
  navigate('packages')
}

/** The pending package request, cleared as it is taken. */
export function takePackageRequest() {
  const request = nav.packageRequest
  nav.packageRequest = null
  return request
}

/** Show Explore at `path`. */
export function openInExplore(path: Path) {
  nav.exploreRequest = path
  navigate('explore')
}

/** The pending Explore path, cleared as it is taken. */
export function takeExploreRequest() {
  const request = nav.exploreRequest
  nav.exploreRequest = null
  return request
}
