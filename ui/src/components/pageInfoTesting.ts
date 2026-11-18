import { flushPromises } from '@vue/test-utils'

/** Opens the PageInfo under `root` and reads its notes, which open in a tooltip on the body. */
export async function openPageInfo(root: { get(selector: string): { trigger(event: string): Promise<void> } }): Promise<string[]> {
  await root.get('[data-testid="page-info"]').trigger('click')
  await flushPromises()
  return [...document.body.querySelectorAll('[role="tooltip"] li')].map(item => item.textContent ?? '')
}

const STATUS = '[data-testid="page-status"]'

/** The page's status (PageStatus) as its button names it, or '' when the page shows none. */
export function pageStatusText(): string {
  return document.querySelector(STATUS)?.getAttribute('aria-label') ?? ''
}

/** The tone the page's status button warns in. */
export function pageStatusTone(): string | null {
  return document.querySelector(STATUS)?.getAttribute('data-tone') ?? null
}

/** Opens the page's status and reads what it shows: the status, its details and the page's notes. */
export async function openPageStatus(): Promise<{ text: string; notes: string[] }> {
  const button = document.querySelector<HTMLElement>(STATUS)
  if (!button) throw new Error('no page status')
  button.click()
  await flushPromises()
  const bubble = document.body.querySelector('[role="tooltip"] .page-status__body')
  return { text: bubble?.textContent ?? '', notes: [...(bubble?.querySelectorAll('li') ?? [])].map(item => item.textContent ?? '') }
}
