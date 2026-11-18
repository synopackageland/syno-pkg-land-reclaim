// Local fixture only; no DSM session or NAS connection.
import { webkit, devices } from 'playwright-core'
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'

const base = process.env.BASE_URL || 'http://127.0.0.1:15144'
const out = resolve(process.env.ARTIFACT_DIR || 'browser-artifacts')
const expected = process.env.EXPECT_LAYOUT || 'mobile'
await mkdir(out, { recursive: true })
const browser = await webkit.launch()
try {
  const context = await browser.newContext({ ...devices['iPhone 15'] })
  await context.addInitScript(() => {
    window.__resizeEvidence = { trustedDropped: 0, synthetic: 0 }
    window.addEventListener('resize', event => {
      if (event.isTrusted) {
        window.__resizeEvidence.trustedDropped++
        event.stopImmediatePropagation()
      } else window.__resizeEvidence.synthetic++
    }, true)
  })
  const page = await context.newPage()
  const errors = []
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(`${base}/browser/portal-layout.html?prepareFullPage=1&lang=cht`)
  await page.locator('[data-ui-layout-mode]').waitFor()
  // Let viewport layout, ResizeObserver and the SDK's next-frame resize settle.
  await page.evaluate(() => new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done))))
  const result = await page.evaluate(() => ({
    innerWidth,
    scrollWidth: document.documentElement.scrollWidth,
    initialHostWidth: Number(document.querySelector('#synoreclaim').dataset.initialWidth),
    hostWidth: document.querySelector('#synoreclaim').getBoundingClientRect().width,
    layoutMode: document.querySelector('[data-ui-layout-mode]').dataset.uiLayoutMode,
    viewport: document.querySelector('meta[name="viewport"]').content,
    resize: window.__resizeEvidence,
  }))
  await page.screenshot({ path: `${out}/iphone-full-page-${expected}.png`, fullPage: true })
  await writeFile(`${out}/iphone-full-page-${expected}.json`, JSON.stringify({ ...result, errors }, null, 2) + '\n')
  console.log(JSON.stringify(result, null, 2))
  assert.deepEqual(errors, [])
  assert(result.initialHostWidth >= 980, 'fixture must first size a desktop-width DSM window')
  assert.equal(result.layoutMode, expected)
  if (expected === 'mobile') {
    assert.equal(result.innerWidth, devices['iPhone 15'].viewport.width)
    assert(result.scrollWidth <= result.innerWidth, 'page must not overflow horizontally')
    assert.equal(result.hostWidth, result.innerWidth)
    assert(result.resize.synthetic > 0, 'SDK must refit DSM without a trusted resize')
  } else {
    assert(result.hostWidth >= 980)
    assert.equal(result.resize.synthetic, 0)
  }
} finally {
  await browser.close()
}
