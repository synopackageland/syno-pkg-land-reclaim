// How long lists of choices are offered (design review 2026-10-01 §1):
// a few as tabs, some as a dropdown, many as a searchable, paged list.

export type ChoiceLayout = 'tabs' | 'dropdown' | 'search'

/** 1–4 volumes keep tabs/cards, 5–20 get one dropdown, more get a searchable select. */
export const VOLUME_TABS_MAX = 4
export const VOLUME_DROPDOWN_MAX = 20
/** A list longer than this gets name search instead of a plain list. */
export const SEARCH_FROM = 20
/** A list longer than this is paged, PAGE_SIZE rows at a time. */
export const PAGE_FROM = 200
export const PAGE_SIZE = 50

export function volumeLayout(count: number): ChoiceLayout {
  if (count <= VOLUME_TABS_MAX) return 'tabs'
  if (count <= VOLUME_DROPDOWN_MAX) return 'dropdown'
  return 'search'
}

/** In the page bar: no picker for one volume, a dropdown up to twenty, then a searchable select. */
export function barVolumeLayout(count: number): ChoiceLayout | null {
  if (count <= 1) return null
  return count <= VOLUME_DROPDOWN_MAX ? 'dropdown' : 'search'
}

export function needsSearch(count: number, from = SEARCH_FROM): boolean {
  return count > from
}

/** How many pages a list needs; one when it is short enough to show whole. */
export function pageCount(count: number): number {
  return count > PAGE_FROM ? Math.ceil(count / PAGE_SIZE) : 1
}

export type PickerOption = {
  value: string
  label: string
  /** The volume or section the option belongs to, shown as a collapsible heading. */
  group?: string
  /** A short note shown next to the label (size, free space, a refusal reason). */
  meta?: string
  /** Why the option cannot be chosen; a truthy value marks it disabled. */
  disabledReason?: string
}

export type OptionGroup = { label: string | null; options: PickerOption[] }

/** Name search: the label only, case-insensitive, trimmed. */
export function matchesQuery(option: PickerOption, query: string): boolean {
  const needle = query.trim().toLocaleLowerCase()
  return !needle || option.label.toLocaleLowerCase().includes(needle)
}

export function filterOptions(options: PickerOption[], query: string): PickerOption[] {
  return options.filter(option => matchesQuery(option, query))
}

/** Options in first-seen group order; an option with no group forms the null group. */
export function groupOptions(options: PickerOption[]): OptionGroup[] {
  const groups = new Map<string | null, PickerOption[]>()
  for (const option of options) {
    const key = option.group ?? null
    const list = groups.get(key)
    if (list) list.push(option)
    else groups.set(key, [option])
  }
  return [...groups].map(([label, grouped]) => ({ label, options: grouped }))
}

export function pageSlice<T>(items: T[], page: number, size = PAGE_SIZE): T[] {
  return items.slice(page * size, page * size + size)
}

/** Values "select all" adds: every option the current filter shows that is not blocked. */
export function selectableValues(options: PickerOption[]): string[] {
  return options.filter(option => option.disabledReason == null).map(option => option.value)
}

/** Merge or replace a selection, depending on whether the list takes many values. */
export function toggledValues(current: string[], value: string, multiple: boolean): string[] {
  if (!multiple) return [value]
  return current.includes(value) ? current.filter(item => item !== value) : [...current, value]
}
