// Dev-only host for the usage-change banner: the real page samples live
// volumes, so this fixture seeds two crossings (one increase, one decrease)
// to show the merged summary. Open /usage-banner.html?lang=zh-TW.
import { createUiApp } from '@/createUiApp'
import UsageBannerFixtureApp from './UsageBannerFixtureApp.vue'
import { usageChange } from './useUsageChange'

usageChange.alerts = [
  { key: 'fixture-volume1', mount: '/volume1', used: 5_000_000_000_000, delta: 50_000_000_000 },
  { key: 'fixture-volume2', mount: '/volume2', used: 3_000_000_000_000, delta: -60_000_000_000 },
]

createUiApp(UsageBannerFixtureApp).mount('#usage-banner-fixture')
