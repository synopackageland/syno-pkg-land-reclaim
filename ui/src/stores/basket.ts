// Cleanup basket per volume: selecting a folder folds
// its selected descendants into it; the sum of reclaimable bytes is a lower
// bound (the exact number comes from cleanup.plan).
import { computed, shallowRef, watch } from 'vue'
import type { Path } from '@/types'
import { readSession, writeSession } from './storage'

export type { ExpectedIdentity } from '@/types/schema/cleanup-plan-params'
import type { ExpectedIdentity } from '@/types/schema/cleanup-plan-params'
export type BasketItem = Path & { reclaimable: number | null; snapshotHeldBytes?: number; expect?: ExpectedIdentity }

/**
 * A duplicate group the Duplicates page added: cleanup.plan gets its id and
 * removes every Remove copy of it on the volume, whichever share it is in,
 * keeping the kept ones. The rest is what Review and the cleanup bar show.
 */
export type DuplicateSelection = {
  id: string
  name: string
  /** Copies removed: every copy on the volume other than the kept ones. */
  copies: number
  canFree: number
  freesLater: number
  /** The kept copy's path from its shared folder; null when it is in a folder the user cannot open. */
  keeps: string
  /** The kept copy is in a preferred share. */
  preferred: boolean
  /** A removed copy is in a Synology Drive folder. */
  drive: boolean
  /**
   * Partly identical folders: each removed folder loses only its `items`
   * identical items; the folder and the items that differ stay.
   */
  partial?: { items: number }
  /**
   * The shared folders holding the copies it removes, when the row named
   * them all; left out when some are unknown.
   */
  shares?: string[]
}

/** The duplicate groups in the list; a share's page only picks which ones it lists. */
export type DuplicatesInBasket = { groups: DuplicateSelection[] }

const SLASH = '2f'
const encoder = new TextEncoder()

/** Keys already computed: every add compares against the whole basket. */
const keys = new WeakMap<Path, string>()

/** Exact path bytes as hex: path_hex when the CLI printed one. */
function pathKey(item: Path) {
  let key = keys.get(item)
  if (key === undefined) {
    key = item.path_hex ?? Array.from(encoder.encode(item.path), b => b.toString(16).padStart(2, '0')).join('')
    keys.set(item, key)
  }
  return key
}

function isWithin(key: string, folderKey: string) {
  return key === folderKey || key.startsWith(folderKey + SLASH)
}

export function createBasket(volume: string) {
  const storageKey = `synoreclaim.basket.${volume}`
  // Replaced on every change, never edited in place: no deep proxies to walk.
  const items = shallowRef<BasketItem[]>(readSession(storageKey, []))
  watch(items, value => writeSession(storageKey, value))

  const has = (path: string | Path) => {
    const key = pathKey(typeof path === 'string' ? { path } : path)
    return items.value.some(i => isWithin(key, pathKey(i)))
  }

  /** false when the item is already covered. A list of any size: the review stages a large one. */
  function add(item: BasketItem) {
    if (has(item)) return false
    const key = pathKey(item)
    const kept = items.value.filter(i => !isWithin(pathKey(i), key))
    items.value = [...kept, item]
    return true
  }

  function remove(path: string | Path) {
    const key = pathKey(typeof path === 'string' ? { path } : path)
    items.value = items.value.filter(i => pathKey(i) !== key)
  }

  /** Whether `item` is `folder` or inside it. */
  const within = (item: Path, folder: Path) => isWithin(pathKey(item), pathKey(folder))

  const duplicates = shallowRef<DuplicatesInBasket>(readSession(`${storageKey}.duplicates`, { groups: [] }))
  watch(duplicates, value => writeSession(`${storageKey}.duplicates`, value))
  const duplicateCopies = computed(() => duplicates.value.groups.reduce((sum, group) => sum + group.copies, 0))
  const duplicateIds = computed(() => new Set(duplicates.value.groups.map(group => group.id)))
  const hasDuplicate = (id: string) => duplicateIds.value.has(id)

  /** Adds groups not in the list yet. */
  function addDuplicates(groups: DuplicateSelection[]) {
    const current = duplicates.value
    const listed = new Set(current.groups.map(group => group.id))
    const fresh = groups.filter(group => !listed.has(group.id) && listed.add(group.id))
    if (fresh.length) duplicates.value = { groups: [...current.groups, ...fresh] }
  }

  function removeDuplicates(ids: Iterable<string>) {
    const gone = new Set(ids)
    const groups = duplicates.value.groups.filter(group => !gone.has(group.id))
    duplicates.value = { groups }
  }

  const clearDuplicates = () => (duplicates.value = { groups: [] })
  const clear = () => {
    items.value = []
    clearDuplicates()
  }
  /** Paths plus duplicate groups: what the cleanup bar counts. */
  const count = computed(() => items.value.length + duplicates.value.groups.length)
  const lowerBound = computed(() => items.value.reduce((sum, i) => sum + (i.reclaimable ?? 0), 0) + duplicates.value.groups.reduce((sum, group) => sum + group.canFree, 0))
  const snapshotHeldBytes = computed(() => items.value.reduce((sum, i) => sum + (i.snapshotHeldBytes ?? 0), 0) + duplicates.value.groups.reduce((sum, group) => sum + group.freesLater, 0))
  const unknownCount = computed(() => items.value.filter(i => i.reclaimable === null).length)

  return { volume, items, has, add, remove, isWithin: within, clear, count, lowerBound, snapshotHeldBytes, unknownCount, duplicates, duplicateCopies, hasDuplicate, addDuplicates, removeDuplicates, clearDuplicates }
}

export type Basket = ReturnType<typeof createBasket>

const baskets = new Map<string, Basket>()

/** The basket of a volume, shared by every page. */
export function useBasket(volume: string) {
  if (!baskets.has(volume)) baskets.set(volume, createBasket(volume))
  return baskets.get(volume)!
}
