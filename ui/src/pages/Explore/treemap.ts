import { hierarchy, treemap, treemapSquarify } from 'd3-hierarchy'
import { t } from '@/i18n'
import type { TopDocument } from '@/types'

export type ExploreEntry = TopDocument['results'][number]['entries'][number]

/** What a tile needs of a row: a top entry, or a share row built without one. */
export type TreemapEntry = Pick<ExploreEntry, 'path' | 'path_hex' | 'service' | 'within'> & {
  kind?: ExploreEntry['kind']
  flags?: string[]
  metrics: { physical: number | null; reclaimable: number | null }
  held_by: ExploreEntry['held_by'] | null
}

type Datum<E> = {
  name: string
  entry: E | null
  physical: number
  /** The area it is laid out with: its size, or the smallest tile that can be clicked. */
  weight: number
  reclaimable: number | null
  snapshotBytes: number
  kind: TileKind
  members: E[]
  children?: Datum<E>[]
}

/**
 * Who manages a tile's bytes, its hue: the user's own files, a package's data,
 * a Recycle Bin, or DSM and everything grouped. How much of it can be freed
 * is the depth of that hue (colorStep), never a second hue.
 */
export type TreemapCategory = 'user' | 'service' | 'recycle' | 'system'

/**
 * What a tile opens: a folder its own level, a file or other item its
 * details, a package's data the Packages page, a group the items in it.
 */
export type TileKind = 'folder' | 'file' | 'service' | 'group'

export type TreemapTile<E extends TreemapEntry = ExploreEntry> = {
  key: string
  entry: E | null
  name: string
  label: string
  /** Where the label starts in the tile: past the kind icon, or 8 px in without it. */
  labelInset: number
  /** Whether the tile draws its kind icon before the label. */
  icon: boolean
  physical: number
  reclaimable: number | null
  snapshotBytes: number
  colorStep: number
  /** Its size among this level's tiles, 0 (smallest) to 1 (largest), on a log scale. */
  sizeRank: number
  category: TreemapCategory
  kind: TileKind
  /** A group's items, largest first; empty for any other tile. */
  members: E[]
  /** Drawn larger than its size, so that it can be seen and clicked. */
  scaled: boolean
  unknown: boolean
  mostlySnapshotHeld: boolean
  x: number
  y: number
  width: number
  height: number
  showLabel: boolean
}

const MAX_TILES = 300

function baseName(path: string) {
  return path.replace(/\/$/, '').split('/').at(-1) || path
}

const SERVICE_IDS = ['synology_drive', 'container_manager', 'surveillance_recordings', 'other_dsm_service_data']

/** Explore's short name of a service, else the package name the Packages page shows. */
export function serviceName(id: string | null | undefined) {
  if (SERVICE_IDS.includes(id ?? '')) return t(`explore.service.${id}`)
  const key = `code.service.${id}`
  const name = id ? t(key) : key
  return name === key ? t('explore.service.unknown') : name
}

export function entryDisplayName(entry: Pick<ExploreEntry, 'path' | 'service'>) {
  return entry.service ? serviceName(entry.service.id) : baseName(entry.path)
}

function pathKey(entry: TreemapEntry) {
  return entry.path_hex ?? Array.from(new TextEncoder().encode(entry.path), byte => byte.toString(16).padStart(2, '0')).join('')
}

function snapshotBytes(entry: TreemapEntry) {
  return (entry.held_by ?? []).reduce((sum, held) => sum + (held.class === 'snapshot' ? held.bytes : 0), 0)
}

function categoryOf(entry: TreemapEntry | null): TreemapCategory {
  if (!entry) return 'system'
  if (entry.service) return 'service'
  if (entry.flags?.includes('recycle')) return 'recycle'
  if (entry.flags?.includes('system')) return 'system'
  return 'user'
}

/** A group's hue: its items' category when they share one, else System and other. */
function groupCategory(members: TreemapEntry[]): TreemapCategory {
  const categories = new Set(members.map(categoryOf))
  return categories.size === 1 ? [...categories][0] : 'system'
}

function colorStep(physical: number, reclaimable: number | null) {
  if (reclaimable == null) return 0
  const ratio = physical > 0 ? Math.max(0, Math.min(1, reclaimable / physical)) : 0
  return Math.min(4, Math.floor(ratio * 5))
}

/** Included rows stay in the list; their bytes already occupy a parent tile. */
export function sizedEntries<E extends TreemapEntry>(entries: E[]) {
  return entries.filter(entry => !entry.within && physicalOf(entry) > 0)
}

function physicalOf(entry: TreemapEntry) {
  return entry.metrics.physical ?? 0
}

/** A folder of its own, as opposed to a file, another kind of item or package data. */
export function isFolder(entry: Pick<TreemapEntry, 'kind' | 'service'>) {
  return entry.kind === 'dir' && !entry.service
}

function tileKind(entry: TreemapEntry): TileKind {
  if (entry.service) return 'service'
  return isFolder(entry) ? 'folder' : 'file'
}

/**
 * The smallest tile, in px², that can be seen, told apart and clicked. Items
 * smaller than this at the map's size are grouped by kind ("12 more files"),
 * and a group or a lone small item is drawn at this size at least.
 */
const MIN_TILE_AREA = 32 * 32

type GroupKind = 'folders' | 'files' | 'items'

function groupKindOf(entry: TreemapEntry): GroupKind {
  if (isFolder(entry)) return 'folders'
  return entry.kind === 'file' && !entry.service ? 'files' : 'items'
}

function sumOrNull(values: (number | null)[]) {
  return values.every(value => value != null) ? values.reduce<number>((sum, value) => sum + (value ?? 0), 0) : null
}

function entryDatum<E extends TreemapEntry>(entry: E, minWeight: number): Datum<E> {
  const physical = physicalOf(entry)
  return {
    name: entryDisplayName(entry),
    entry,
    physical,
    weight: Math.max(physical, minWeight),
    reclaimable: entry.metrics.reclaimable,
    snapshotBytes: snapshotBytes(entry),
    kind: tileKind(entry),
    members: [],
  }
}

/** Items of a known size here, largest first; a row inside another row adds no area. */
function placedEntries<E extends TreemapEntry>(entries: E[]) {
  return entries
    .filter(entry => !entry.within && entry.metrics.physical != null && entry.metrics.physical >= 0)
    .sort((left, right) => physicalOf(right) - physicalOf(left) || left.path.localeCompare(right.path))
}

function makeData<E extends TreemapEntry>(entries: E[], area: number): Datum<E>[] {
  const placed = placedEntries(entries)
  const total = placed.reduce((sum, entry) => sum + physicalOf(entry), 0)
  if (total <= 0) return []
  // Items under the clickable size are grouped by kind; past MAX_TILES the rest are too.
  const minWeight = area > 0 ? total * Math.min(1, MIN_TILE_AREA / area) : 0
  const shown: Datum<E>[] = []
  const grouped = new Map<GroupKind, E[]>()
  for (const entry of placed) {
    if (physicalOf(entry) >= minWeight && shown.length < MAX_TILES - 3) {
      shown.push(entryDatum(entry, 0))
      continue
    }
    const kind = groupKindOf(entry)
    grouped.set(kind, [...grouped.get(kind) ?? [], entry])
  }
  for (const [kind, members] of grouped) {
    // A lone small item is its own tile, drawn at the clickable size.
    if (members.length === 1) {
      shown.push(entryDatum(members[0], minWeight))
      continue
    }
    const physical = members.reduce((sum, entry) => sum + physicalOf(entry), 0)
    shown.push({
      name: t(`explore.treemap.more.${kind}`, { count: members.length }),
      entry: null,
      physical,
      weight: Math.max(physical, minWeight),
      reclaimable: sumOrNull(members.map(entry => entry.metrics.reclaimable)),
      snapshotBytes: members.reduce((sum, entry) => sum + snapshotBytes(entry), 0),
      kind: 'group',
      members,
    })
  }
  return shown
}

/** A label's drawn width in px. */
export type MeasureLabel = (text: string) => number

/** Without a measured font: 7 px a character. */
const estimateLabel: MeasureLabel = text => text.length * 7

/** The name, or its longest start with an ellipsis that fits in `room` px. */
function fitLabel(name: string, room: number, measure: MeasureLabel): string {
  if (measure(name) <= room) return name
  const chars = [...name]
  let end = chars.length - 1
  while (end > 1 && measure(`${chars.slice(0, end).join('')}…`) > room) end -= 1
  return `${chars.slice(0, end).join('')}…`
}

/**
 * The label a tile of `width` px draws, where it starts and whether the kind
 * icon goes before it. The name in full comes first: a name that fits only
 * without the icon is drawn without it; otherwise a wide tile keeps its icon
 * and the name's longest start that fits. The label keeps 8 px from the right edge.
 */
export function placeLabel(name: string, width: number, measure: MeasureLabel = estimateLabel) {
  const icon = width >= ICON_MIN_WIDTH
  const length = measure(name)
  if (icon && length <= width - ICON_INSET - 8) return { label: name, labelInset: ICON_INSET, icon: true }
  if (length <= width - 16) return { label: name, labelInset: 8, icon: false }
  const inset = icon ? ICON_INSET : 8
  return { label: fitLabel(name, width - inset - 8, measure), labelInset: inset, icon }
}

function labelFits(width: number, height: number) {
  return width >= 60 && height >= 24
}

export type MapSize = { width: number; height: number }

/**
 * A laid-out size stays in use while the map is within this factor of it, so
 * a height change (the cleanup bar showing, a pane being dragged) stretches
 * the tiles in place instead of reordering them.
 */
const RELAYOUT_FACTOR = 1.35

export function needsRelayout(laidOut: MapSize, size: MapSize) {
  if (laidOut.width <= 0 || laidOut.height <= 0) return true
  const drift = (from: number, to: number) => Math.max(from, to) / Math.max(1, Math.min(from, to))
  return drift(laidOut.width, size.width) > RELAYOUT_FACTOR || drift(laidOut.height, size.height) > RELAYOUT_FACTOR
}

/**
 * The same tiles, stretched from the size they were laid out at to `size`,
 * each label fitted again to the tile it is now drawn in.
 */
export function scaleTiles<E extends TreemapEntry>(tiles: TreemapTile<E>[], laidOut: MapSize, size: MapSize, measure: MeasureLabel = estimateLabel): TreemapTile<E>[] {
  if (laidOut.width === size.width && laidOut.height === size.height) return tiles
  const sx = laidOut.width > 0 ? size.width / laidOut.width : 1
  const sy = laidOut.height > 0 ? size.height / laidOut.height : 1
  return tiles.map(tile => {
    const width = tile.width * sx
    const height = tile.height * sy
    return { ...tile, ...placeLabel(tile.name, width, measure), x: tile.x * sx, y: tile.y * sy, width, height, showLabel: labelFits(width, height) }
  })
}

type Box = { x: number; y: number; width: number; height: number }

/** The CSS transform that blows `tile` up to fill a map of `size` (origin at the map's corner). */
export function zoomTransform(tile: Box, size: MapSize) {
  const sx = size.width / Math.max(1, tile.width)
  const sy = size.height / Math.max(1, tile.height)
  return `translate(${-tile.x * sx}px, ${-tile.y * sy}px) scale(${sx}, ${sy})`
}

/** A zoom's length: longer the deeper it goes, kept short so it never holds up browsing. */
export function zoomDuration(tile: MapSize, size: MapSize) {
  const ratio = Math.sqrt((size.width * size.height) / Math.max(1, tile.width * tile.height))
  return Math.round(Math.min(250, 130 + 45 * Math.log2(Math.max(1, ratio))))
}

/** A tile at least this wide draws its kind icon before the label. */
export const ICON_MIN_WIDTH = 88

/** A label's left inset past the 12 px kind icon. */
const ICON_INSET = 26

export function layoutTreemap<E extends TreemapEntry>(entries: E[], width: number, height: number, measure: MeasureLabel = estimateLabel): TreemapTile<E>[] {
  const children = makeData(entries, Math.max(0, width) * Math.max(0, height))
  // Without children the root is the only leaf: an empty 0 B tile.
  if (!children.length) return []
  const root = hierarchy<Datum<E>>({ name: '', entry: null, physical: 0, weight: 0, reclaimable: 0, snapshotBytes: 0, kind: 'group', members: [], children })
    .sum(node => node.children ? 0 : node.weight)
    .sort((left, right) => (right.value ?? 0) - (left.value ?? 0) || left.data.name.localeCompare(right.data.name))

  const laidOutRoot = treemap<Datum<E>>()
    .size([Math.max(0, width), Math.max(0, height)])
    .tile(treemapSquarify)
    .paddingInner(2)
    .round(true)(root)

  const sizes = children.map(child => Math.log1p(child.physical))
  const [smallest, largest] = [Math.min(...sizes), Math.max(...sizes)]
  const sizeRank = (physical: number) => largest > smallest ? (Math.log1p(physical) - smallest) / (largest - smallest) : 1
  return laidOutRoot.leaves().map(node => {
    const data = node.data
    const tileWidth = node.x1 - node.x0
    const tileHeight = node.y1 - node.y0
    const key = data.entry ? pathKey(data.entry) : `group:${data.members[0] ? groupKindOf(data.members[0]) : data.name}`
    return {
      key,
      entry: data.entry,
      name: data.name,
      ...placeLabel(data.name, tileWidth, measure),
      physical: data.physical,
      reclaimable: data.reclaimable,
      snapshotBytes: data.snapshotBytes,
      colorStep: colorStep(data.physical, data.reclaimable),
      sizeRank: sizeRank(data.physical),
      category: data.kind === 'group' ? groupCategory(data.members) : categoryOf(data.entry),
      kind: data.kind,
      members: data.members,
      scaled: data.weight > data.physical,
      unknown: data.reclaimable == null,
      mostlySnapshotHeld: data.physical > 0 && data.snapshotBytes >= data.physical / 2,
      x: node.x0,
      y: node.y0,
      width: tileWidth,
      height: tileHeight,
      showLabel: labelFits(tileWidth, tileHeight),
    }
  }).sort((left, right) => {
    const leftPath = left.entry?.path ?? left.name
    const rightPath = right.entry?.path ?? right.name
    return leftPath.localeCompare(rightPath)
  })
}
