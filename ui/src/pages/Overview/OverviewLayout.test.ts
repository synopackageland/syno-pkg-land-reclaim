import source from './OverviewPage.vue?raw'
import donut from '@/components/Donut.vue?raw'

// jsdom has no layout, so pin the rule: "Where the space went" spans the
// Overview's width. The estimate card that shared its row is gone (seen on a test NAS),
// and a two-column row would leave an empty column beside it.
describe('Overview top row', () => {
  it('gives the space panel the whole row', () => {
    expect(source).not.toMatch(/class="(top-row|hero)"/)
    expect(source).toMatch(/\n {8}<SpacePanel /)
  })

  it('wraps the actions heading so its watch button stays inside', () => {
    expect(source).toMatch(/<Space\b[^>]*\bwrap\b[^>]*class="actions-head"/)
  })

  it('puts the legend under the donut when the panel is too narrow for both', () => {
    expect(donut).toMatch(/\.donut-chart \{[^}]*flex-wrap: wrap;/)
    expect(donut).toMatch(/\.legend \{[^}]*flex: 1 1 260px;/)
  })
})
