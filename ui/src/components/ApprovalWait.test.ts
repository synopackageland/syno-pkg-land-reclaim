import {mount} from '@vue/test-utils'
import {nextTick} from 'vue'
import {catalogs} from '@/i18n/catalogs'
import {createTranslator} from '@/i18n/t'
import {reportAuthorizationProgress,resetAuthorization} from '@/stores/authorization'
import ApprovalWait from './ApprovalWait.vue'

beforeEach(()=>resetAuthorization())

describe('ApprovalWait', () => {
 it('stays hidden while the approval is younger than 30 s', async () => {
  const wrapper = mount(ApprovalWait)
  reportAuthorizationProgress({op:'bootstrap',state:'running',elapsedMs:29999})
  await nextTick()
  expect(wrapper.find('.approval-wait').exists()).toBe(false)
 })

 it('tells the administrator to keep waiting once the task passes 30 s', async () => {
  const wrapper = mount(ApprovalWait)
  reportAuthorizationProgress({op:'bootstrap',state:'running',elapsedMs:95000})
  await nextTick()
  const note = wrapper.find('.approval-wait')
  expect(note.attributes('role')).toBe('status')
  expect(note.text()).toContain('Still waiting for DSM')
  expect(note.text()).toContain('The approval task is running')
  expect(note.text()).toContain('1:35')
 })

 it('says when DSM has not started the task yet', async () => {
  const wrapper = mount(ApprovalWait)
  reportAuthorizationProgress({op:'bootstrap',state:'queued',elapsedMs:30000})
  await nextTick()
  expect(wrapper.text()).toContain('waiting for DSM to start it')
 })

 it('has Traditional Chinese copy', () => {
  const {t} = createTranslator('zh-TW', catalogs)
  expect(t('approvalWait.title')).toBe('仍在等待 DSM')
  expect(t('approvalWait.running')).toContain('核准任務正在執行')
  expect(t('approvalWait.queued')).toContain('尚未開始')
  expect(t('approvalWait.elapsed', {duration:'1:35'})).toBe('已等待 1:35')
 })
})
