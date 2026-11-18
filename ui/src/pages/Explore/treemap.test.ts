import { mount } from '@vue/test-utils'
import { layoutTreemap, scaleTiles, type TreemapEntry } from './treemap'
import Treemap from './Treemap.vue'

function entry(path: string, physical: number): TreemapEntry {
  return { path, service: null, metrics: { physical, reclaimable: physical / 2 }, held_by: [] }
}

const entries = [entry('/volume1/a', 600), entry('/volume1/b', 300), entry('/volume1/c', 100)]

describe('Treemap zoom', () => {
  let animate: ReturnType<typeof vi.fn>
  beforeEach(() => {
    animate = vi.fn(() => ({ cancel: vi.fn() }))
    ;(SVGElement.prototype as unknown as { animate: unknown }).animate = animate
  })
  afterEach(() => {
    delete (SVGElement.prototype as unknown as { animate?: unknown }).animate
    vi.unstubAllGlobals()
  })

  it('zooms into a folder tile while its level loads, without holding up the activation', async () => {
    const wrapper = mount(Treemap, { props: { entries, zoomable: () => true } })
    await wrapper.findAll('g[role="button"]')[0].trigger('click')
    expect(wrapper.emitted('activate')).toHaveLength(1)
    expect(animate).toHaveBeenCalledTimes(1)
    const [frames, options] = animate.mock.calls[0] as [Keyframe[], KeyframeAnimationOptions]
    expect(frames[0].transform).toBe('none')
    expect(String(frames[1].transform)).toMatch(/^translate\(.+\) scale\(.+\)$/)
    expect(options.duration).toBeLessThanOrEqual(250)
  })

  it('zooms out of the tile the previous level was', async () => {
    const wrapper = mount(Treemap, { props: { entries: [entry('/volume1/x', 10)], focusKey: null } })
    const key = Array.from(new TextEncoder().encode('/volume1/b'), byte => byte.toString(16).padStart(2, '0')).join('')
    await wrapper.setProps({ entries, focusKey: key })
    await new Promise(resolve => setTimeout(resolve))
    expect(animate).toHaveBeenCalledTimes(1)
    const [frames] = animate.mock.calls[0] as [Keyframe[]]
    expect(frames[1].transform).toBe('none')
  })

  it('does not zoom a tile that opens nothing, or when motion is reduced', async () => {
    const plain = mount(Treemap, { props: { entries, zoomable: () => false } })
    await plain.findAll('g[role="button"]')[0].trigger('click')
    vi.stubGlobal('matchMedia', (query: string) => ({ matches: query.includes('reduce') }))
    const reduced = mount(Treemap, { props: { entries, zoomable: () => true } })
    await reduced.findAll('g[role="button"]')[0].trigger('click')
    expect(animate).not.toHaveBeenCalled()
    expect(reduced.emitted('activate')).toHaveLength(1)
  })
})

describe('Treemap tiles of one level', () => {
  const big = (path: string, physical: number, kind: TreemapEntry['kind']): TreemapEntry => ({ ...entry(path, physical), kind })
  const level = [
    big('/volume1/logs/daemon.log_0', 5 * 1024 * 1024, 'file'),
    ...['a', 'b', 'c'].map(name => big(`/volume1/logs/${name}`, 4096, 'dir')),
    ...Array.from({ length: 40 }, (_, index) => big(`/volume1/logs/f${index}.log`, 4096, 'file')),
    big('/volume1/logs/empty', 0, 'dir'),
    big('/volume1/logs/lonely.sock', 4096, 'socket'),
  ]

  it('groups items too small to click by kind, and keeps every one of them', () => {
    const tiles = layoutTreemap(level, 800, 500)
    expect(tiles.map(tile => [tile.name, tile.kind]).sort()).toEqual([
      ['4 more folders', 'group'],
      ['40 more files', 'group'],
      ['daemon.log_0', 'file'],
      ['lonely.sock', 'file'],
    ])
    const members = tiles.flatMap(tile => tile.members.length ? tile.members : [tile.entry!])
    expect(members.map(item => item.path).sort()).toEqual(level.map(item => item.path).sort())
  })

  it('draws a group or a lone small item no smaller than a clickable tile', () => {
    const tiles = layoutTreemap(level, 800, 500)
    for (const tile of tiles) expect(tile.width * tile.height).toBeGreaterThanOrEqual(600)
    expect(tiles.find(tile => tile.name === 'lonely.sock')).toMatchObject({ scaled: true })
    expect(tiles.find(tile => tile.name === 'daemon.log_0')).toMatchObject({ scaled: false })
  })

  it('tells a folder from a file and from package data', () => {
    const tiles = layoutTreemap([
      big('/volume1/s/dir', 600, 'dir'),
      big('/volume1/s/file', 300, 'file'),
      { ...big('/volume1/s/@appstore', 300, 'service'), service: { id: 'synology_drive' } as TreemapEntry['service'] },
    ], 800, 500)
    expect(Object.fromEntries(tiles.map(tile => [tile.entry!.path.split('/').at(-1), tile.kind]))).toEqual({ dir: 'folder', file: 'file', '@appstore': 'service' })
  })
})

describe('Treemap groups', () => {
  const level = [
    { ...entry('/volume1/logs/daemon.log_0', 5 * 1024 * 1024), kind: 'file' as const },
    ...['a', 'b', 'c'].map(name => ({ ...entry(`/volume1/logs/${name}`, 4096), kind: 'dir' as const })),
  ]

  it('opens a group as its own items and goes back to the whole level', async () => {
    const wrapper = mount(Treemap, { props: { entries: level } })
    const group = wrapper.find('g[data-kind="group"]')
    expect(group.attributes('aria-label')).toMatch(/^3 more folders:/)
    await group.trigger('click')
    expect(wrapper.emitted('activate')).toBeUndefined()
    expect(wrapper.findAll('g[role="button"]').map(tile => tile.attributes('data-kind'))).toEqual(['folder', 'folder', 'folder'])
    await wrapper.find('button').trigger('click')
    expect(wrapper.findAll('g[role="button"]')).toHaveLength(2)
  })

  it('closes an open group when a new level arrives', async () => {
    const wrapper = mount(Treemap, { props: { entries: level } })
    await wrapper.find('g[data-kind="group"]').trigger('click')
    await wrapper.setProps({ entries: [...level] })
    expect(wrapper.find('button').exists()).toBe(false)
    expect(wrapper.find('g[data-kind="group"]').exists()).toBe(true)
  })
})

// B14: the DSM window opens small and is then maximized. The map keeps its
// arrangement and stretches it, but each label must fit the tile it is drawn in.
describe('Treemap labels after the map changes size', () => {
  const measure = (text: string) => text.length * 7
  const named = [entry('/volume1/iSCSI LUN storage for VMs', 600), entry('/volume1/b', 300), entry('/volume1/c', 100)]
  const label = (tiles: ReturnType<typeof layoutTreemap>, name: string) => tiles.find(tile => tile.name === name)!
  /** Where a label ends: past its inset, before the tile's 8 px right margin. */
  const fits = (tile: ReturnType<typeof layoutTreemap>[number]) =>
    !tile.showLabel || tile.labelInset + measure(tile.label) <= tile.width - 8

  it('fits each label again when the map grows or shrinks without a new arrangement', () => {
    const laidOut = layoutTreemap(named, 200, 200, measure)
    expect(label(laidOut, 'iSCSI LUN storage for VMs').label.endsWith('…')).toBe(true)

    const grown = scaleTiles(laidOut, { width: 200, height: 200 }, { width: 320, height: 200 }, measure)
    expect(label(grown, 'iSCSI LUN storage for VMs').label).toBe('iSCSI LUN storage for VMs')
    const shrunk = scaleTiles(laidOut, { width: 200, height: 200 }, { width: 150, height: 200 }, measure)
    for (const tile of [...grown, ...shrunk]) expect(fits(tile), `${tile.label} in ${tile.width}`).toBe(true)
  })

  it('leaves the kind icon out rather than cutting a name that fits without it', () => {
    // 9 characters, 63 px: 7 px short beside the icon of a 96 px tile, room enough without it.
    const [tile] = layoutTreemap([entry('/volume1/iSCSI LUN', 100)], 96, 120, measure)
    expect(tile).toMatchObject({ label: 'iSCSI LUN', labelInset: 8, icon: false })
    const [wide] = layoutTreemap([entry('/volume1/iSCSI LUN', 100)], 200, 120, measure)
    expect(wide).toMatchObject({ label: 'iSCSI LUN', labelInset: 26, icon: true })
  })
})
