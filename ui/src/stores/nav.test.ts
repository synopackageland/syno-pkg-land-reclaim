import { navigate, nav, openInExplore, openPackage, restoredPage, takeExploreRequest, takePackageRequest } from './nav'

describe('nav store', () => {
  it('persists the page in sessionStorage', () => {
    navigate('explore')
    expect(nav.page).toBe('explore')
    expect(restoredPage()).toBe('explore')
  })

  it('opens a package on the Packages page and hands the request over once', () => {
    openPackage('hyper_backup', '/volume1/a.hbk')
    expect(nav.page).toBe('packages')
    expect(takePackageRequest()).toEqual({ service: 'hyper_backup', path: '/volume1/a.hbk' })
    expect(takePackageRequest()).toBeNull()
  })

  it('opens a path in Explore and hands the request over once', () => {
    openInExplore({ path: '/volume1/@docker', path_hex: 'aa' })
    expect(nav.page).toBe('explore')
    expect(takeExploreRequest()).toEqual({ path: '/volume1/@docker', path_hex: 'aa' })
    expect(takeExploreRequest()).toBeNull()
  })
})
