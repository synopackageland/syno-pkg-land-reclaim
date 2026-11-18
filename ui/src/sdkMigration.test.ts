import { flushPromises, mount } from '@vue/test-utils'
import { AppRoot, Button, Dialog } from '@synopackageland/ui'
import { defineComponent, h } from 'vue'
import stylesSource from './styles.ts?raw'
import { mount as mountApp } from './dsmBundle'

it('contains modals in the app without changing host body styles or hiding a sibling', async () => {
  const before = document.body.getAttribute('style')
  document.body.style.margin = '13px'
  const sibling = document.createElement('button')
  sibling.textContent = 'Host app'
  document.body.append(sibling)
  const host = defineComponent({ render: () => h(AppRoot, { style: 'height:560px;width:700px' }, {
    default: () => h(Dialog, { open: true, title: 'Confirmation', description: 'Confirm this action' }, { default: () => 'Body' }),
  }) })
  const wrapper = mount(host, { attachTo: document.body })
  try {
    await flushPromises()
    const panel = wrapper.element.querySelector('[role="dialog"]')!
    expect(panel).not.toBeNull()
    expect(panel.getAttribute('aria-modal')).not.toBe('true')
    expect(sibling.getAttribute('aria-hidden')).toBeNull()
    expect(sibling.style.pointerEvents).toBe('')
    expect(document.body.style.margin).toBe('13px')
  } finally {
    wrapper.unmount()
    expect(document.body.style.margin).toBe('13px')
    sibling.remove()
    if (before === null) document.body.removeAttribute('style')
    else document.body.setAttribute('style', before)
  }
})

it('gives independently mounted DSM apps distinct Vue id prefixes', async () => {
  const roots = [document.createElement('div'), document.createElement('div')]
  roots.forEach(root => document.body.append(root))
  const apps = roots.map(root => mountApp(root))
  try {
    await flushPromises()
    const prefixes = roots.map(root => (root as unknown as { __vue_app__: { config: { idPrefix: string } } }).__vue_app__.config.idPrefix)
    expect(prefixes.every(Boolean)).toBe(true)
    expect(new Set(prefixes).size).toBe(2)
  } finally {
    apps.forEach(app => app.unmount())
    roots.forEach(root => root.remove())
  }
})

it('uses the renamed SDK DOM namespace', () => {
  const wrapper = mount(AppRoot, { slots: { default: () => h(Button, {}, () => 'Action') } })
  try {
    expect(wrapper.attributes()).toHaveProperty('data-syno-pkg-land-ui')
    expect(wrapper.find('[data-syno-ui]').exists()).toBe(false)
  } finally { wrapper.unmount() }
})

it('uses the shared embedded UI and SDK styles without an app reset', () => {
  const source = stylesSource
  const imports = [...source.matchAll(/import '([^']+)'/g)].map(match => match[1])
  expect(imports).toEqual(['@synopackageland/ui/themes/dsm7-embedded.css', '@synopackageland/sdk/vue/style.css'])
})
