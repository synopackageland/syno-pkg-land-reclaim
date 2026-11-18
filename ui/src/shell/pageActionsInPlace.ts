import { defineComponent, h } from 'vue'
import { config } from '@vue/test-utils'

/** Where a page mounted without AppShell shows its PageActions. */
export const PAGE_ACTIONS = '[data-page-actions]'

const InPlace = defineComponent((_, { slots }) => () => h('div', { 'data-page-actions': '' }, slots.default?.()))

/**
 * AppShell's page header takes a page's PageActions, and the SDK exports no
 * target of its own; a test of one page shows them in place instead.
 */
export function showPageActionsInPlace() {
  beforeAll(() => { config.global.stubs.PageActions = InPlace })
  afterAll(() => { delete config.global.stubs.PageActions })
}
