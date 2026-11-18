// Every page of the app, already complete: page threads edit only their
// own folder under src/pages/, never this file or the shell (src/shell/).
import { defineAsyncComponent, type Component } from 'vue'
import { FolderSearch, History, Camera, Copy, LayoutDashboard, Trash2, Boxes, Settings } from '@lucide/vue'

export type PageDef = {
  key: string
  /** i18n key of the sidebar label and page title. */
  titleKey: string
  /** Pages with an icon are listed in the sidebar, in this order. */
  icon?: Component
  component: Component
  /** The page fills the height under the header instead of scrolling. */
  fill?: boolean
  /** 'bottom': listed after every other page, in a group of its own. */
  placement?: 'bottom'
}

export const pages: PageDef[] = [
  { key: 'overview', titleKey: 'overview.title', icon: LayoutDashboard, component: defineAsyncComponent(() => import('./Overview/OverviewPage.vue')) },
  { key: 'explore', titleKey: 'explore.title', icon: FolderSearch, fill: true, component: defineAsyncComponent(() => import('./Explore/ExplorePage.vue')) },
  { key: 'duplicates', titleKey: 'duplicates.title', icon: Copy, fill: true, component: defineAsyncComponent(() => import('./Duplicates/DuplicatesPage.vue')) },
  { key: 'snapshots', titleKey: 'snapshots.title', icon: Camera, component: defineAsyncComponent(() => import('./Snapshots/SnapshotsPage.vue')) },
  { key: 'recycle', titleKey: 'recycle.title', icon: Trash2, fill: true, component: defineAsyncComponent(() => import('./Recycle/RecyclePage.vue')) },
  { key: 'packages', titleKey: 'packages.title', icon: Boxes, fill: true, component: defineAsyncComponent(() => import('./Packages/PackagesPage.vue')) },
  { key: 'history', titleKey: 'history.title', icon: History, fill: true, component: defineAsyncComponent(() => import('./History/HistoryPage.vue')) },
  { key: 'scanning', titleKey: 'scan.title', component: defineAsyncComponent(() => import('./Scanning/ScanningPage.vue')) },
  { key: 'cleanupApply', titleKey: 'review.applying.title', component: defineAsyncComponent(() => import('./Review/CleanupApplyPage.vue')) },
  { key: 'rootfsApply', titleKey: 'rootfsApply.title', component: defineAsyncComponent(() => import('./RootfsApply/RootfsApplyPage.vue')) },
  { key: 'settings', titleKey: 'settings.title', icon: Settings, fill: true, placement: 'bottom', component: defineAsyncComponent(() => import('./Settings/SettingsPage.vue')) },
]

export const NewScanDialog = defineAsyncComponent(() => import('./NewScan/NewScanDialog.vue'))
export const JobStrip = defineAsyncComponent(() => import('@/components/JobStrip.vue'))
export const ReviewDialog = defineAsyncComponent(() => import('./Review/ReviewDialog.vue'))
