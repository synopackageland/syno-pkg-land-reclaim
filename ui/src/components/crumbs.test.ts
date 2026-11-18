import type { Path } from '@/types'
import { crumbChain, crumbName } from './crumbs'

const volume: Path = { path: '/volume1' }

describe('crumbChain', () => {
  it('is the volume alone at its root', () => {
    expect(crumbChain(volume, volume)).toEqual([volume])
  })

  it('adds one crumb per path segment below the volume', () => {
    expect(crumbChain(volume, { path: '/volume1/share04/folder0005' })).toEqual([
      volume,
      { path: '/volume1/share04' },
      { path: '/volume1/share04/folder0005' },
    ])
  })

  it('ignores trailing slashes on either path', () => {
    expect(crumbChain({ path: '/volume1/' }, { path: '/volume1/share04/' })).toEqual([
      { path: '/volume1/' },
      { path: '/volume1/share04' },
    ])
  })

  it('keeps the exact bytes of a non-UTF-8 folder in every ancestor', () => {
    const target: Path = { path: '/volume1/caf\uFFFD/f\uFFFDle', path_hex: '2f766f6c756d65312f636166ff2f66ff6c65' }

    expect(crumbChain(volume, target)).toEqual([
      volume,
      { path: '/volume1/caf\uFFFD', path_hex: '2f766f6c756d65312f636166ff' },
      target,
    ])
  })

  it('falls back to the display text when the hex does not name the same path', () => {
    const target: Path = { path: '/volume1/caf\uFFFD', path_hex: '2f9f' }

    expect(crumbChain(volume, target)).toEqual([volume, { path: '/volume1/caf\uFFFD' }])
  })

  it('shows the volume root for a target outside the volume', () => {
    expect(crumbChain(volume, { path: '/volume2/share04' })).toEqual([volume])
  })

  it('is the target alone without a volume, and empty with neither', () => {
    expect(crumbChain(null, { path: '/volume1/share04' })).toEqual([{ path: '/volume1/share04' }])
    expect(crumbChain(null, null)).toEqual([])
  })
})

describe('crumbName', () => {
  it('names a crumb by its last path segment', () => {
    expect(crumbName('/volume1/share04/')).toBe('share04')
    expect(crumbName('/')).toBe('/')
  })
})
