/* global taskSpace, layoutConfig */
// Run through ego-browser nodejs with layoutConfig = { spaceId, page, baseUrl,
// artifactDir }. Uses the existing task space and fixture-only NewScanDialog.
const fs = await import('node:fs/promises')
const assert = (await import('node:assert/strict')).default
const task = await taskSpace(layoutConfig.spaceId)
const page = task.page(layoutConfig.page)
const evidence = []
const locales = ['enu', 'cht', 'chs', 'jpn', 'krn', 'ger', 'fre', 'spn', 'ita']
await page.cdp('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false })
for (const view of ['overview', 'explore']) {
for (const lang of locales) {
  for (const [width, height, zoom] of [[1100, 720, 1], [800, 560, 1], [800, 560, 1.25], [700, 560, 1], [1350, 850, 1]]) {
    await page.goto(`${layoutConfig.baseUrl}/browser/dsm-layout.html?lang=${lang}&width=${width}&height=${height}&scenario=normal&view=${view}`)
    await page.waitForSelector(view === 'overview' ? '.hero' : '.explore-layout')
    await page.evaluate(z => { document.querySelector('#window').style.zoom = z }, zoom)
    const tooltipBounds = {}
    for (const interaction of ['hover', 'focus']) {
      if (interaction === 'hover') {
        await page.hover('[data-ui-app] .app-page-actions [tabindex="0"]')
      } else {
        await page.mouse.move(10, 10)
        await page.focus('[data-ui-page-header] button:first-child')
        await page.focus('[data-ui-app] .app-page-actions [tabindex="0"]')
      }
      await page.waitForSelector('[data-ui-app] [role="tooltip"]')
      const bounds = await page.evaluate(() => {
        const root = document.querySelector('[data-ui-app]')
        const tooltip = root.querySelector('[role="tooltip"]').parentElement
        return { root: root.getBoundingClientRect().toJSON(), tooltip: tooltip.getBoundingClientRect().toJSON() }
      })
      assert(bounds.tooltip.width > 1 && bounds.tooltip.height > 1, `${lang}/${width}/${zoom}/${interaction}: tooltip is not visible content`)
      assert(bounds.tooltip.top >= bounds.root.top && bounds.tooltip.bottom <= bounds.root.bottom && bounds.tooltip.left >= bounds.root.left && bounds.tooltip.right <= bounds.root.right, `${lang}/${width}/${zoom}/${interaction}: status details clipped`)
      tooltipBounds[interaction] = bounds
    }
    if (view === 'explore') {
      evidence.push({ view, lang, width, height, zoom, tooltipBounds })
      continue
    }
    // The header's last action is New Scan. Never press Scan.
    await page.click('[data-ui-page-header] button:last-child')
    await page.waitForSelector('[data-ui-app] [role="dialog"]')
    // Exercise a long displayed name while keeping the native component tree.
    await page.evaluate(() => {
      const name = document.querySelector('[data-ui-app] [role="dialog"] [role="radiogroup"] label > span > span:first-child')
      if (!name) throw new Error('volume radio label was not rendered')
      name.textContent = 'Very long shared folder and volume name '.repeat(80)
    })
    const geometry = await page.evaluate(() => {
      const root = document.querySelector('[data-ui-app]')
      const panel = root.querySelector('[role="dialog"]')
      const rect = element => element.getBoundingClientRect().toJSON()
      return { root: rect(root), panel: rect(panel), header: rect(panel.querySelector('header')), footer: rect(panel.querySelector('footer')), body: rect(panel.children[1]), scrollHeight: panel.children[1].scrollHeight, clientHeight: panel.children[1].clientHeight }
    })
    for (const part of ['panel', 'header', 'footer']) {
      const r = geometry[part], host = geometry.root
      assert(r.top >= host.top - 1 && r.bottom <= host.bottom + 1, `${lang}/${width}/${zoom}: ${part} outside window vertically`)
      assert(r.left >= host.left - 1 && r.right <= host.right + 1, `${lang}/${width}/${zoom}: ${part} outside window horizontally`)
    }
    await page.mouse.move(geometry.body.left + 100, geometry.body.top + 80)
    await page.mouse.wheel(0, 700, { label: 'scroll scan settings body' })
    await page.waitForFunction(() => document.querySelector('[data-ui-app] [role="dialog"] > div').scrollTop > 0)
    const wheelScroll = await page.evaluate(() => document.querySelector('[data-ui-app] [role="dialog"] > div').scrollTop)
    await page.evaluate(() => {
      const body = document.querySelector('[data-ui-app] [role="dialog"] > div')
      body.scrollTop = 0
    })
    // RadioGroup uses roving focus: PageDown selects the last radio.
    // A checkbox leaves PageDown to native scrolling of the dialog body.
    await page.focus('[data-ui-app] [role="dialog"] [role="checkbox"] >> nth=0')
    await page.evaluate(() => { document.querySelector('[data-ui-app] [role="dialog"] > div').scrollTop = 0 })
    await page.keyboard.press('PageDown')
    await page.waitForFunction(() => document.querySelector('[data-ui-app] [role="dialog"] > div').scrollTop > 0)
    const keyboardScroll = await page.evaluate(() => document.querySelector('[data-ui-app] [role="dialog"] > div').scrollTop)
    evidence.push({ view, lang, width, height, zoom, geometry, tooltipBounds, wheelScroll, keyboardScroll })
    if (lang === 'cht' || lang === 'ger') await page.screenshot({ path: `${layoutConfig.artifactDir}/dialog-${lang}-${width}-zoom${zoom}.png` })
    await page.keyboard.press('Escape')
  }
}
}
await fs.writeFile(`${layoutConfig.artifactDir}/browser-layout-results.json`, JSON.stringify(evidence, null, 2))
console.log({ cases: evidence.length, passed: true })
