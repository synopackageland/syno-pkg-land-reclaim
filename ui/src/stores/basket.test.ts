import { nextTick } from 'vue'
import { createBasket } from './basket'

const item = (path: string, reclaimable: number | null = 100) => ({ path, reclaimable })

describe('basket', () => {
  beforeEach(() => sessionStorage.clear())

  it('sums reclaimable bytes as a lower bound and counts unknowns', () => {
    const basket = createBasket('/volume1')
    basket.add(item('/volume1/a', 100))
    basket.add(item('/volume1/b', null))
    expect(basket.lowerBound.value).toBe(100)
    expect(basket.unknownCount.value).toBe(1)
  })

  it('sums snapshot-held bytes for the basket hint', () => {
    const basket = createBasket('/volume1')
    basket.add({ ...item('/volume1/share/a', 100), snapshotHeldBytes: 40 })
    basket.add({ ...item('/volume1/share/b', 80), snapshotHeldBytes: 25 })
    expect(basket.snapshotHeldBytes.value).toBe(65)
  })

  it('folds selected descendants into a selected folder', () => {
    const basket = createBasket('/volume1')
    basket.add(item('/volume1/share/a/x'))
    basket.add(item('/volume1/share/ab'))
    basket.add(item('/volume1/share/a'))
    expect(basket.items.value.map(i => i.path)).toEqual(['/volume1/share/ab', '/volume1/share/a'])
  })

  it('ignores an item already covered by a selected folder', () => {
    const basket = createBasket('/volume1')
    basket.add(item('/volume1/share'))
    expect(basket.add(item('/volume1/share/a'))).toBe(false)
    expect(basket.has('/volume1/share/a')).toBe(true)
    expect(basket.items.value).toHaveLength(1)
  })

  it('compares exact bytes when the CLI printed path_hex', () => {
    const basket = createBasket('/volume1')
    basket.add({ path: '/volume1/�', path_hex: '2f766f6c756d65312fff', reclaimable: 1 })
    basket.add({ path: '/volume1/�', path_hex: '2f766f6c756d65312ffe', reclaimable: 1 })
    expect(basket.items.value).toHaveLength(2)
  })

  it('holds a list of any size: a large one is staged for the plan', () => {
    const basket = createBasket('/volume1')
    for (let i = 0; i < 1500; i++) basket.add(item(`/volume1/s/${i}`))
    expect(basket.add(item('/volume1/s/extra'))).toBe(true)
    expect(basket.items.value).toHaveLength(1501)
    const groups = Array.from({ length: 3000 }, (_, i) => ({ id: i.toString(16).padStart(32, '0'), name: `g${i}`, copies: 2, canFree: 1, freesLater: 0, keeps: '/k', preferred: false, drive: false }))
    basket.addDuplicates(groups)
    expect(basket.duplicates.value.groups).toHaveLength(3000)
  })

  it('tells whether an item is a folder or inside it', () => {
    const basket = createBasket('/volume1')
    expect(basket.isWithin({ path: '/volume1/photos/e' }, { path: '/volume1/photos' })).toBe(true)
    expect(basket.isWithin({ path: '/volume1/videos/d' }, { path: '/volume1/video' })).toBe(false)
  })

  it('persists per volume in sessionStorage', async () => {
    createBasket('/volume1').add(item('/volume1/a'))
    await nextTick()
    expect(createBasket('/volume1').items.value).toHaveLength(1)
    expect(createBasket('/volume2').items.value).toHaveLength(0)
  })

  it('removes and clears', () => {
    const basket = createBasket('/volume1')
    basket.add(item('/volume1/a'))
    basket.add(item('/volume1/b'))
    basket.remove('/volume1/a')
    expect(basket.items.value.map(i => i.path)).toEqual(['/volume1/b'])
    basket.clear()
    expect(basket.items.value).toEqual([])
  })
})
