import { defineComponent, ref } from 'vue'
import { flushPromises, mount } from '@vue/test-utils'
import { AppRoot, ToastProvider } from '@synopackageland/ui'
import { jobs } from '@/stores/jobs'
import RootfsCleanupDialog from './RootfsCleanupDialog.vue'
import { rootfsCleanupFixtureBackend } from './cleanupFixtureBackend'
import { selectRootfsFixtureScenario } from '@/pages/RootfsApply/rootfsApplyFixtureBackend'
import { setBackend } from '@/api'

const Host = defineComponent({
  components: { AppRoot, ToastProvider, RootfsCleanupDialog },
  setup() {
    const dialog = ref<{ open: () => Promise<void> } | null>(null)
    return { dialog }
  },
  template: `
    <AppRoot>
      <ToastProvider>
        <RootfsCleanupDialog ref="dialog" />
      </ToastProvider>
    </AppRoot>
  `,
})

async function clickTextButton(wrapper: ReturnType<typeof mount>, label: string) {
  const button = wrapper.findAll('button').find(candidate => candidate.text() === label)
  if (!button) throw new Error(`button not found: ${label}`)
  await button.trigger('click')
}

describe('rootfs cleanup fixture backend', () => {
  it('keeps the apply job running after U3 confirms the plan', async () => {
    document.body.innerHTML = ''
    sessionStorage.clear()
    jobs.list = []
    selectRootfsFixtureScenario('moves')
    setBackend(rootfsCleanupFixtureBackend('review_cleanup'))
    const wrapper = mount(Host, { attachTo: document.body })

    await wrapper.vm.dialog?.open()
    await flushPromises()
    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.text()).toContain('What will be moved'))
    await wrapper.find('[role="checkbox"]').trigger('click')
    await clickTextButton(wrapper, 'Yes, clean up')

    await vi.waitFor(() => expect(wrapper.text()).toContain('Moving files: 2 of 3'))
    expect(wrapper.text()).not.toContain('Couldn’t show cleanup progress')
    wrapper.unmount()
  })
})
