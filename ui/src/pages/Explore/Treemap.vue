<script setup lang="ts" generic="E extends TreemapEntry">
import { computed, nextTick, ref, shallowRef, watch } from 'vue'
import { Button } from '@synopackageland/ui'
import { ArrowLeft, File, Files, Folder, Folders, Layers, Settings2 } from '@lucide/vue'
import { useElementSize } from '@/composables/useElementSize'
import { formatBytes, formatCount, t } from '@/i18n'
import { isFolder, layoutTreemap, needsRelayout, scaleTiles, zoomDuration, zoomTransform, type MapSize, type MeasureLabel, type TreemapEntry, type TreemapTile } from './treemap'
import { textMeasure } from './measureText'

const props = withDefaults(defineProps<{
  entries: E[]
  selectedKeys?: string[]
  /** Tiles that open a level of their own; activating one zooms into it while it loads. */
  zoomable?: (entry: E) => boolean
  /** The tile the previous, deeper level was; a new level zooms out of it. */
  focusKey?: string | null
}>(), { selectedKeys: () => [], zoomable: undefined, focusKey: null })

const emit = defineEmits<{ activate: [entry: E] }>()
const mapHost = ref<SVGSVGElement | null>(null)
const tilesHost = ref<SVGGElement | null>(null)
const size = useElementSize(mapHost, { width: 470, height: 440 })
// Labels are measured in the font they are drawn in (12 px semibold).
const measureLabel = computed<MeasureLabel | undefined>(() =>
  mapHost.value ? textMeasure(`600 12px ${getComputedStyle(mapHost.value).fontFamily}`) : undefined)

// A group tile opens its items on the map, at their own sizes; Back returns
// to the whole level, and a new level closes any group.
const openGroups = shallowRef<TreemapTile<E>[]>([])
const openGroup = computed(() => openGroups.value.at(-1) ?? null)
const shownEntries = computed(() => openGroup.value?.members ?? props.entries)
watch(() => props.entries, () => { openGroups.value = [] })

// The arrangement is laid out once per level and then stretched to the map's
// size, so the cleanup bar or a dragged pane never reshuffles the tiles; only
// new entries, or a size far from the laid-out one, lay them out again.
const laidOutSize = shallowRef<MapSize>({ width: 0, height: 0 })
const laidOut = shallowRef<TreemapTile<E>[]>([])
function relayout() {
  const current = { ...size.value }
  laidOutSize.value = current
  laidOut.value = layoutTreemap(shownEntries.value, current.width, current.height, measureLabel.value)
}
watch([shownEntries, measureLabel], relayout, { immediate: true })
// The first measurement replaces the placeholder size: lay out for the real map.
let measured = false
watch(size, value => {
  if (!measured || needsRelayout(laidOutSize.value, value)) relayout()
  measured = true
})
const tiles = computed(() => scaleTiles(laidOut.value, laidOutSize.value, size.value, measureLabel.value))
const selected = computed(() => new Set(props.selectedKeys))

// Category hues are DSM's own; depth says how much can be freed. Within a
// step, a larger tile is a shade deeper (less than half a step, so steps stay
// apart). A folder's title bar is the same hue deeper still; a file is flat.
const HUES = { user: 'var(--ui-blue-9)', service: 'var(--ui-green-9)', recycle: 'var(--ui-orange-9)', system: 'var(--ui-gray-11)' } as const
const DEPTH = [12, 23, 34, 45, 56]
const SIZE_SHADE = 5
const BAR_DEPTH = 20
/** A folder's title bar, where its name fits on it. */
const BAR_HEIGHT = 22

function depthOf(tile: TreemapTile<E>) {
  const step = tile.unknown ? DEPTH[0] : DEPTH[tile.colorStep]
  return step + Math.round((tile.sizeRank - 0.5) * SIZE_SHADE)
}

function mix(tile: TreemapTile<E>, depth: number) {
  return `color-mix(in oklab, ${HUES[tile.category]} ${depth}%, var(--ui-surface))`
}

function fillFor(tile: TreemapTile<E>) {
  return tile.kind === 'group' ? mix(tile, 8) : mix(tile, depthOf(tile))
}

function barFor(tile: TreemapTile<E>) {
  return mix(tile, Math.min(76, depthOf(tile) + BAR_DEPTH))
}

function hatchFor(tile: TreemapTile<E>) {
  if (tile.mostlySnapshotHeld) return 'url(#explore-snapshot-hatch)'
  if (tile.unknown) return 'url(#explore-unknown-hatch)'
  return null
}

/** A folder with room for its name has a title bar; a smaller one keeps a strip of it. */
function barHeight(tile: TreemapTile<E>) {
  if (tile.kind !== 'folder' || tile.width < 8 || tile.height < 8) return 0
  return tile.showLabel && tile.height >= BAR_HEIGHT + 6 ? BAR_HEIGHT : Math.min(4, tile.height / 4)
}

/** A rectangle with its top corners rounded: a folder's title bar. */
function barPath(tile: TreemapTile<E>, height: number) {
  const r = Math.min(4, height, tile.width / 2)
  const { x, y, width } = tile
  return `M${x},${y + height}V${y + r}a${r},${r} 0 0 1 ${r},${-r}H${x + width - r}a${r},${r} 0 0 1 ${r},${r}V${y + height}Z`
}

const ICONS = { folder: Folder, file: File, service: Settings2 } as const
function iconFor(tile: TreemapTile<E>) {
  if (tile.kind !== 'group') return ICONS[tile.kind]
  if (tile.members.every(isFolder)) return Folders
  return tile.members.every(entry => entry.kind === 'file' && !entry.service) ? Files : Layers
}

/** The first line's baseline: on a folder's title bar, else near the tile's top. */
function nameY(tile: TreemapTile<E>) {
  return tile.y + (barHeight(tile) === BAR_HEIGHT ? 15 : 18)
}

function sizeY(tile: TreemapTile<E>) {
  return tile.y + (barHeight(tile) === BAR_HEIGHT ? BAR_HEIGHT + 15 : 34)
}

function showsSize(tile: TreemapTile<E>) {
  return tile.showLabel && sizeY(tile) + 6 <= tile.y + tile.height
}

/** What a click does where the tile has room: always on a group, else on hover and focus. */
function hint(tile: TreemapTile<E>) {
  return t(`explore.treemap.hint.${tile.kind}`)
}

function showsHint(tile: TreemapTile<E>) {
  return tile.showLabel && tile.width >= 120 && tile.height >= 64
}

function reducedMotion() {
  return typeof window.matchMedia === 'function' && window.matchMedia('(prefers-reduced-motion: reduce)').matches
}

let zoom: Animation | null = null
let zoomTimer: ReturnType<typeof setTimeout> | undefined
function endZoom() {
  zoom?.cancel()
  zoom = null
  clearTimeout(zoomTimer)
}

/** Zoom the map between the whole level and one tile; skipped without motion or the Web Animations API. */
function playZoom(tile: TreemapTile<E>, direction: 'in' | 'out') {
  const host = tilesHost.value
  if (!host || typeof host.animate !== 'function' || reducedMotion()) return
  endZoom()
  const zoomed = zoomTransform(tile, size.value)
  const frames = direction === 'in'
    ? [{ transform: 'none', opacity: 1 }, { transform: zoomed, opacity: 0.35 }]
    : [{ transform: zoomed, opacity: 0.35 }, { transform: 'none', opacity: 1 }]
  zoom = host.animate(frames, { duration: zoomDuration(tile, size.value), easing: 'cubic-bezier(0.33, 1, 0.68, 1)', fill: direction === 'in' ? 'forwards' : 'none' })
  // A zoom into a level that never arrives (a failed query) does not stay.
  if (direction === 'in') zoomTimer = setTimeout(endZoom, 1500)
}

// A new level replaces the zoomed-in one; coming back up, it zooms out of the tile left.
watch(() => props.entries, async () => {
  endZoom()
  if (!props.focusKey) return
  await nextTick()
  const from = tiles.value.find(tile => tile.key === props.focusKey)
  if (from) playZoom(from, 'out')
})

function activate(tile: TreemapTile<E>) {
  if (tile.kind === 'group') {
    openGroups.value = [...openGroups.value, tile]
    return
  }
  if (!tile.entry) return
  if (props.zoomable?.(tile.entry)) playZoom(tile, 'in')
  emit('activate', tile.entry)
}

function closeGroup() {
  openGroups.value = openGroups.value.slice(0, -1)
}

function handleKey(tile: TreemapTile<E>, event: KeyboardEvent) {
  if (event.key !== 'Enter' && event.key !== ' ') return
  event.preventDefault()
  activate(tile)
}

function itemLabel(tile: TreemapTile<E>) {
  return t('explore.treemap.item', { name: tile.name, physical: formatBytes(tile.physical), reclaimable: formatBytes(tile.reclaimable) })
}

/** The tooltip: what the tile is, what a click does, and when it is drawn larger than its size. */
function tooltip(tile: TreemapTile<E>) {
  return [itemLabel(tile), hint(tile), tile.scaled ? t('explore.treemap.scaled') : ''].filter(Boolean).join('\n')
}
</script>

<template>
  <div class="flex h-full min-h-0 min-w-0 w-full flex-col gap-1.5">
    <div v-if="openGroup" class="explore-treemap-group flex min-w-0 items-center gap-2">
      <Button size="sm" variant="ghost" @click="closeGroup">
        <ArrowLeft class="size-3.5" aria-hidden="true" />{{ t('explore.treemap.back') }}
      </Button>
      <span class="min-w-0 truncate text-[12px] text-[var(--ui-text-secondary)]">{{ openGroup.name }} · {{ formatBytes(openGroup.physical) }}</span>
    </div>
    <svg
      ref="mapHost"
      class="explore-treemap block min-h-0 min-w-0 w-full flex-1 overflow-hidden rounded-[var(--ui-radius-lg)] bg-[var(--ui-surface-subtle)]"
      :viewBox="`0 0 ${size.width} ${size.height}`"
      preserveAspectRatio="none"
      role="group"
      :aria-label="t('explore.treemap.aria', { count: formatCount(tiles.length) })"
    >
      <defs>
        <pattern id="explore-snapshot-hatch" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
          <rect width="2" height="6" fill="var(--ui-text)" opacity="0.16" />
        </pattern>
        <pattern id="explore-unknown-hatch" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(-45)">
          <rect width="1.5" height="6" fill="var(--ui-text)" opacity="0.1" />
        </pattern>
      </defs>
      <g ref="tilesHost" class="explore-treemap-tiles">
        <g
          v-for="tile in tiles"
          :key="tile.key"
          :role="tile.entry || tile.kind === 'group' ? 'button' : 'img'"
          :tabindex="tile.entry || tile.kind === 'group' ? 0 : undefined"
          :aria-label="itemLabel(tile)"
          :aria-description="hint(tile)"
          :data-category="tile.category"
          :data-kind="tile.kind"
          :data-selected="selected.has(tile.key) ? '' : undefined"
          :style="{ '--tile-hue': HUES[tile.category] }"
          :class="[
            'explore-tile cursor-pointer',
            'outline-none focus-visible:[&>rect:first-of-type]:stroke-[var(--ui-focus-ring)] focus-visible:[&>rect:first-of-type]:stroke-[3]',
          ]"
          @click="activate(tile)"
          @keydown="handleKey(tile, $event)"
        >
          <title>{{ tooltip(tile) }}</title>
          <rect
            class="explore-tile-fill"
            :x="tile.x"
            :y="tile.y"
            :width="tile.width"
            :height="tile.height"
            rx="4"
            :style="{ fill: fillFor(tile) }"
            :stroke="selected.has(tile.key) ? 'var(--ui-primary)' : tile.kind === 'group' ? 'var(--ui-border-strong)' : 'var(--ui-surface)'"
            :stroke-width="selected.has(tile.key) ? 2.5 : tile.kind === 'group' ? 1 : 1.5"
            :stroke-dasharray="tile.kind === 'group' && !selected.has(tile.key) ? '4 3' : undefined"
          />
          <rect
            v-if="hatchFor(tile)"
            :x="tile.x"
            :y="tile.y"
            :width="tile.width"
            :height="tile.height"
            rx="4"
            :fill="hatchFor(tile)!"
            pointer-events="none"
          />
          <path
            v-if="barHeight(tile)"
            class="explore-tile-bar"
            :d="barPath(tile, barHeight(tile))"
            :style="{ fill: barFor(tile) }"
            pointer-events="none"
          />
          <component
            :is="iconFor(tile)"
            v-if="tile.showLabel && tile.icon"
            :x="tile.x + 8"
            :y="nameY(tile) - 10"
            :size="12"
            :stroke-width="2"
            class="text-[var(--ui-text-secondary)]"
            aria-hidden="true"
            pointer-events="none"
          />
          <text
            v-if="tile.showLabel"
            :x="tile.x + tile.labelInset"
            :y="nameY(tile)"
            fill="var(--ui-text)"
            font-size="12"
            font-weight="600"
            pointer-events="none"
          >{{ tile.label }}</text>
          <text
            v-if="showsSize(tile)"
            :x="tile.x + tile.labelInset"
            :y="sizeY(tile)"
            fill="var(--ui-text-secondary)"
            font-size="11"
            class="tabular-nums"
            pointer-events="none"
          >{{ formatBytes(tile.physical) }}</text>
          <text
            v-if="showsHint(tile)"
            :class="tile.kind === 'group' ? undefined : 'explore-tile-hint'"
            :x="tile.x + 8"
            :y="tile.y + tile.height - 9"
            fill="var(--ui-text-secondary)"
            font-size="11"
            pointer-events="none"
          >{{ hint(tile) }} ›</text>
        </g>
      </g>
    </svg>
  </div>
</template>

<style scoped>
.explore-treemap-tiles {
  transform-origin: 0 0;
}
.explore-tile-fill {
  transition: filter 120ms ease, stroke 120ms ease;
}
.explore-tile:hover > .explore-tile-fill {
  filter: brightness(0.97);
  stroke: color-mix(in oklab, var(--tile-hue) 60%, var(--ui-surface));
}
.explore-tile-hint {
  opacity: 0;
  transition: opacity 120ms ease;
}
.explore-tile:is(:hover, :focus-visible) > .explore-tile-hint {
  opacity: 1;
}
@media (prefers-reduced-motion: reduce) {
  .explore-tile-fill, .explore-tile-hint { transition: none; }
}
</style>
