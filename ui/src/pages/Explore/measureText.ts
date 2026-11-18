/**
 * Measures text in a CSS `font` as the browser draws it. Without a canvas
 * (jsdom) there is nothing to measure with, and callers keep their estimate.
 */
export function textMeasure(font: string): ((text: string) => number) | undefined {
  const context = typeof OffscreenCanvas === 'undefined' ? null : new OffscreenCanvas(1, 1).getContext('2d')
  if (!context) return undefined
  context.font = font
  return text => context.measureText(text).width
}

/** The width a classic vertical scrollbar takes from its box; 0 with overlay scrollbars. */
export function scrollbarWidth(): number {
  if (typeof document === 'undefined') return 0
  const probe = document.createElement('div')
  probe.style.cssText = 'position:absolute;visibility:hidden;width:100px;height:100px;overflow:scroll'
  document.body.append(probe)
  const width = probe.offsetWidth - probe.clientWidth
  probe.remove()
  return width
}
