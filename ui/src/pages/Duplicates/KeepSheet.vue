<script setup lang="ts">
// One group's copies, opened from its row: every identical copy on the
// volume, whichever shared folder it is in, with a box per copy ticked for
// the copies that move to the Recycle Bin. One copy starts unticked (the
// rule's kept one, or those the user kept), the others ticked; the user can
// untick more to keep several, and at least one stays. Copies no cleanup can
// move (right below the volume, in a share without a Recycle Bin, or moved
// with an identical folder) are listed without a box, with the reason. Apply
// stores the copies left unticked as kept (duplicates.keep) and puts the
// group in the cleanup list. A merged row (identical folders that keep the
// same folder) lists its member folders instead, with how much of each matches.
import { computed, ref, watch } from 'vue'
import { Button, Checkbox, SegmentControl, SegmentItem, Sheet, Tag } from '@synopackageland/ui'
import { Star } from '@lucide/vue'
import FileStationButton from '@/components/FileStationButton.vue'
import ShareSettingsButton from '@/components/ShareSettingsButton.vue'
import { fileStationBlock, fileStationBlockKey } from '@/components/fileStation'
import { formatBytes, formatCount, formatDateTime, joinParts, locale, t } from '@/i18n'
import type { RecycleBins } from '@/dsm/recycleBins'
import { isFolderKind, type DuplicateCopy, type DuplicateGroup } from '@/types/duplicates'
import { allCopies } from './duplicatesClient'
import {
  copyBlock, copyKey, copyKind, copyLocation, deletedForGood, keepPlan, keeperReasonText, mergedMember, partialText, removalOf, rowName, shareOf, unmovableText,
  type CopyBlock, type DuplicateRow, type KeepChange, type Removal,
} from './duplicatesModel'
import PartialItems from './PartialItems.vue'

const props = defineProps<{
  row: DuplicateRow | null
  volume: string
  /** The scope folder for the API; the volume when left out. */
  scopePath?: string
  /** Groups in the cleanup list. */
  selected: ReadonlySet<string>
  /** Numbers are recalculated, or a keep is being stored: nothing changes. */
  busy?: boolean
  /** Each share's Recycle Bin; null when DSM did not say. */
  bins: RecycleBins | null
}>()
const open = defineModel<boolean>('open', { default: false })
const emit = defineEmits<{
  /** Store the keeps, then put the group in the cleanup list. */
  apply: [group: DuplicateGroup, changes: KeepChange[]]
  /** A merged row: the member groups to have in the cleanup list. */
  applyMembers: [add: DuplicateGroup[], drop: DuplicateGroup[]]
  drop: [groups: DuplicateGroup[]]
  fileStation: [path: string, group: DuplicateGroup]
}>()

const group = computed(() => props.row?.group ?? null)
const merged = computed(() => (props.row?.members.length ?? 0) > 1)

// One group: all its copies on the volume.
const copies = ref<DuplicateCopy[]>([])
const loading = ref(false)
const failed = ref(false)
/** Keys of the copies ticked to move. */
const ticked = ref(new Set<string>())
let request = 0
async function load(current: DuplicateGroup) {
  const id = ++request
  loading.value = true
  failed.value = false
  try {
    const answer = await allCopies({ volume: props.volume, group_id: current.group.id })
    if (id !== request) return
    copies.value = answer.copies
    ticked.value = new Set(answer.copies.filter(copy => copy.role === 'remove' && !block(copy)).map(copy => copyKey(copy.path)))
  } catch {
    if (id === request) failed.value = true
  } finally {
    if (id === request) loading.value = false
  }
}

// A merged row: each member folder, ticked when it moves.
const members = computed(() => (props.row?.members ?? []).map(member => mergedMember(member)))
const tickedMembers = ref(new Set<string>())

watch(() => [open.value, props.row] as const, ([isOpen, row], before) => {
  if (!isOpen || !row) return
  const sameRow = before?.[1]?.id === row.id && before[0]
  if (row.members.length > 1) {
    if (!sameRow) tickedMembers.value = new Set(row.members.filter(member => member.removes > 0).map(member => member.group.id))
    return
  }
  if (!sameRow) {
    copies.value = []
    ticked.value = new Set()
  }
  void load(row.group)
}, { immediate: true })

function block(copy: DuplicateCopy): CopyBlock | null {
  return group.value ? copyBlock(copy, group.value, props.volume) : null
}
function blockText(copy: DuplicateCopy, why: CopyBlock): string {
  if (why === 'unmovable') return unmovableText(copy.path, props.volume, props.bins)
  return t(deletedForGood(copy, props.bins) ? 'duplicates.block.inFolderDelete' : 'duplicates.block.inFolder')
}

// Oldest first by default: the oldest copy is usually the one kept.
const order = ref<'oldest' | 'newest'>('oldest')
const time = (copy: DuplicateCopy) => Date.parse(copy.mtime) || 0
const sorted = computed(() => [...copies.value].sort((a, b) => (order.value === 'oldest' ? time(a) - time(b) : time(b) - time(a)) || a.path.path.localeCompare(b.path.path)))
function changeOrder(value: string | undefined) {
  if (value === 'oldest' || value === 'newest') order.value = value
}

/** The copies left unticked, those without a box included: what stays. */
const keptKeys = computed(() => new Set(copies.value.filter(copy => copy.role !== 'in_folder' && !ticked.value.has(copyKey(copy.path))).map(copy => copyKey(copy.path))))
const plan = computed(() => group.value ? keepPlan(group.value, copies.value, keptKeys.value, props.volume) : null)
const moving = computed(() => copies.value.filter(copy => ticked.value.has(copyKey(copy.path))).length)
const counted = computed(() => copies.value.filter(copy => copy.role !== 'in_folder').length)
function tick(copy: DuplicateCopy, value: boolean) {
  const next = new Set(ticked.value)
  if (value) next.add(copyKey(copy.path))
  else next.delete(copyKey(copy.path))
  ticked.value = next
}
const isDefaultKept = (copy: DuplicateCopy) => copy.role === 'keep'
function detail(copy: DuplicateCopy) {
  const date = formatDateTime(copy.mtime)
  return joinParts([date, isDefaultKept(copy) ? keeperReasonText(group.value!.keeper.reason) : ''].filter(Boolean), locale)
}
function fileStationReason(path: string, current: DuplicateGroup) {
  const blocked = fileStationBlock({ path, kind: copyKind(current.group) })
  return blocked ? t(fileStationBlockKey(blocked)) : ''
}

const isFolder = computed(() => !!group.value && isFolderKind(group.value.group.kind))
const title = computed(() => props.row ? (merged.value ? props.row.name : rowName(props.row.group)) : '')
const description = computed(() => {
  const current = group.value
  if (!current) return ''
  if (merged.value) return props.row!.hereText
  if (current.partial) return joinParts([partialText(current, props.volume), t('duplicates.partial.stays', { name: rowName(current) })], locale)
  if (isFolder.value) return t('duplicates.sheet.folderDescription', { count: current.group.files ?? 0, files: formatCount(current.group.files ?? 0), size: formatBytes(current.group.size) })
  return t('duplicates.sheet.size', { size: formatBytes(current.group.size) })
})
/**
 * What the ticked copies come to: moved to a Recycle Bin, deleted for good,
 * or some of each, by their shared folders' Recycle Bins. Before any is
 * ticked, the copies that could be.
 */
const memberShare = (path: { path: string }) => shareOf(path.path, props.volume)
const removal = computed<Removal>(() => {
  if (merged.value) {
    const chosen = members.value.filter(member => tickedMembers.value.has(member.group.group.id))
    return removalOf((chosen.length ? chosen : members.value).map(member => memberShare(member.path)), props.bins)
  }
  const choosable = copies.value.filter(copy => !block(copy))
  const chosen = choosable.filter(copy => ticked.value.has(copyKey(copy.path)))
  return removalOf((chosen.length ? chosen : choosable).map(copy => copy.share), props.bins)
})
const SUMMARY = { move: 'duplicates.sheet.summary', delete: 'duplicates.sheet.summaryDelete', mixed: 'duplicates.sheet.summaryMixed' } as const
const LEAD = { move: 'duplicates.sheet.lead', delete: 'duplicates.sheet.leadDelete', mixed: 'duplicates.sheet.leadMixed' } as const
const MERGED_LEAD = { move: 'duplicates.merged.lead', delete: 'duplicates.merged.leadDelete', mixed: 'duplicates.merged.leadMixed' } as const
const summary = computed(() => merged.value
  ? t(SUMMARY[removal.value], { moves: formatCount(tickedMembers.value.size), total: formatCount(members.value.length + 1), kept: formatCount(members.value.length + 1 - tickedMembers.value.size) })
  : t(SUMMARY[removal.value], { moves: formatCount(moving.value), total: formatCount(counted.value), kept: formatCount(counted.value - moving.value) }))
/** A ticked copy with no Recycle Bin: it is deleted for good. A kept one is not removed at all. */
const deletesHere = (copy: DuplicateCopy) => ticked.value.has(copyKey(copy.path)) && deletedForGood(copy, props.bins)

const inList = computed(() => (props.row?.members ?? []).some(member => props.selected.has(member.group.id)))
const noneStays = computed(() => !merged.value && !loading.value && copies.value.length > 0 && plan.value === null)
const canApply = computed(() => !props.busy && !loading.value && !failed.value && (merged.value ? tickedMembers.value.size > 0 : plan.value !== null && moving.value > 0))
function apply() {
  if (!canApply.value || !props.row) return
  if (merged.value) {
    const add = props.row.members.filter(member => tickedMembers.value.has(member.group.id))
    const drop = props.row.members.filter(member => !tickedMembers.value.has(member.group.id))
    emit('applyMembers', add, drop)
  } else emit('apply', props.row.group, plan.value!)
  open.value = false
}
function tickMember(id: string, value: boolean) {
  const next = new Set(tickedMembers.value)
  if (value) next.add(id)
  else next.delete(id)
  tickedMembers.value = next
}
</script>

<template>
  <Sheet v-model:open="open" size="640px" :title="title" :description="description" :close-label="t('common.close')">
    <div v-if="row && group" class="keep-sheet" data-testid="keep-sheet">
      <template v-if="merged">
        <p class="keep-sheet__lead">{{ t(MERGED_LEAD[removal]) }}</p>
        <ul class="keep-sheet__copies" data-testid="keep-members">
          <li class="keep-sheet__copy--kept" data-testid="keep-member-kept">
            <span class="keep-sheet__label">
              <span class="keep-sheet__path">{{ copyLocation(group.keeper.path, group.group, volume) }}</span>
              <span class="keep-sheet__muted">{{ joinParts([formatDateTime(group.keeper.mtime), keeperReasonText(group.keeper.reason)].filter(Boolean), locale) }}</span>
            </span>
            <Tag :icon="Star" :label="t('duplicates.merged.kept')" appearance="success" />
            <FileStationButton :reason="fileStationReason(group.keeper.path.path, group)" @open="emit('fileStation', group.keeper.path.path, group)" />
          </li>
          <li v-for="member in members" :key="member.group.group.id" data-testid="keep-member">
            <Checkbox
              class="keep-sheet__label"
              :model-value="tickedMembers.has(member.group.group.id)"
              :disabled="busy || !member.group.removes"
              :label="copyLocation(member.path, member.group.group, volume)"
              :description="joinParts([member.same, member.mtime ? formatDateTime(member.mtime) : ''].filter(Boolean), locale)"
              @update:model-value="value => tickMember(member.group.group.id, !!value)"
            />
            <FileStationButton :reason="fileStationReason(member.path.path, member.group)" @open="emit('fileStation', member.path.path, member.group)" />
          </li>
        </ul>
      </template>

      <template v-else>
        <div class="keep-sheet__head">
          <p class="keep-sheet__lead">{{ t(LEAD[removal]) }}</p>
          <SegmentControl :model-value="order" :aria-label="t('duplicates.sheet.sort')" size="sm" data-testid="keep-order" @update:model-value="changeOrder">
            <SegmentItem value="oldest">{{ t('duplicates.sheet.oldest') }}</SegmentItem>
            <SegmentItem value="newest">{{ t('duplicates.sheet.newest') }}</SegmentItem>
          </SegmentControl>
        </div>
        <p v-if="failed" class="keep-sheet__muted" role="alert">{{ t('duplicates.state.failed') }}</p>
        <ul class="keep-sheet__copies" :aria-busy="loading || undefined" data-testid="keep-copies">
          <li
            v-for="copy in sorted" :key="copyKey(copy.path)"
            :class="{ 'keep-sheet__copy--kept': !ticked.has(copyKey(copy.path)) }"
            :data-testid="block(copy) ? `keep-copy-blocked-${block(copy)}` : ticked.has(copyKey(copy.path)) ? 'keep-copy-moves' : 'keep-copy-kept'"
          >
            <span v-if="block(copy)" class="keep-sheet__label">
              <span class="keep-sheet__path">{{ copyLocation(copy.path, group.group, volume) }}</span>
              <span class="keep-sheet__muted">{{ joinParts([detail(copy), blockText(copy, block(copy)!)].filter(Boolean), locale) }}</span>
            </span>
            <Checkbox
              v-else
              class="keep-sheet__label"
              :model-value="ticked.has(copyKey(copy.path))"
              :disabled="busy"
              :label="copyLocation(copy.path, group.group, volume)"
              :description="joinParts([detail(copy), deletesHere(copy) ? t('duplicates.sheet.deletes') : ''].filter(Boolean), locale)"
              @update:model-value="value => tick(copy, !!value)"
            />
            <ShareSettingsButton v-if="!block(copy) && deletesHere(copy) && bins?.get(copy.share)?.enabled === false" :share="copy.share" />
            <Tag v-if="isDefaultKept(copy)" :icon="Star" :label="t('duplicates.keep.kept')" appearance="success" data-testid="keep-default" />
            <FileStationButton :reason="fileStationReason(copy.path.path, group)" @open="emit('fileStation', copy.path.path, group)" />
          </li>
        </ul>
        <p v-if="noneStays" class="keep-sheet__warn" role="alert" data-testid="keep-at-least-one">{{ t('duplicates.sheet.atLeastOne') }}</p>
        <PartialItems v-if="group.partial" :group="group" :volume="volume" :scope-path="scopePath" />
      </template>
    </div>
    <template v-if="row" #footer>
      <div class="keep-sheet__footer">
        <span class="keep-sheet__summary" data-testid="keep-summary">{{ summary }}</span>
        <Button @click="open = false">{{ t('common.close') }}</Button>
        <Button v-if="inList" data-testid="keep-drop" @click="emit('drop', row.members); open = false">{{ t('duplicates.keep.drop') }}</Button>
        <Button variant="primary" :disabled="!canApply" data-testid="keep-apply" @click="apply">{{ inList ? t('duplicates.sheet.update') : t('duplicates.sheet.apply') }}</Button>
      </div>
    </template>
  </Sheet>
</template>

<style scoped>
.keep-sheet {
  display: grid;
  gap: 10px;
}
.keep-sheet__head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.keep-sheet__lead {
  margin: 0;
  font-weight: 600;
  color: var(--ui-text);
}
.keep-sheet__copies {
  margin: 0;
  padding: 0;
  list-style: none;
  border: 1px solid var(--ui-border);
  border-radius: var(--ui-radius-md);
}
.keep-sheet__copies li {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  padding: 8px 10px;
  border-top: 1px solid var(--ui-border);
}
.keep-sheet__copies li:first-child {
  border-top: 0;
}
.keep-sheet__copy--kept {
  background: var(--ui-surface-subtle);
}
.keep-sheet__label {
  display: grid;
  flex: 1 1 240px;
  min-width: 0;
}
.keep-sheet__path {
  overflow-wrap: anywhere;
}
.keep-sheet__muted {
  color: var(--ui-text-secondary);
  font-size: 12px;
}
.keep-sheet__warn {
  margin: 0;
  color: var(--ui-danger-text);
}
.keep-sheet__footer {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  width: 100%;
}
.keep-sheet__summary {
  flex: 1 1 auto;
  min-width: 0;
  color: var(--ui-text-secondary);
}
</style>
