// Geometry of the hand-written SVG charts.

/**
 * Stroke-dasharray arcs of a donut: each value's share of `circumference`. A
 * value above zero gets at least `minLength`, taken from the larger arcs, so a
 * tiny part stays visible; the legend keeps the exact figures.
 */
export function donutArcs(values: number[], circumference: number, { minLength = 0 } = {}) {
  const lengths = arcLengths(values, circumference, minLength)
  let start = 0
  return lengths.map(length => {
    const arc = { length, offset: start === 0 ? 0 : -start }
    start += length
    return arc
  })
}

function arcLengths(values: number[], circumference: number, minLength: number) {
  const shown = values.map((value, i) => (value > 0 ? i : -1)).filter(i => i >= 0)
  const raised = new Set<number>()
  for (;;) {
    const rest = circumference - minLength * raised.size
    const free = shown.filter(i => !raised.has(i))
    const sum = free.reduce((a, i) => a + values[i], 0)
    if (!free.length || rest <= 0 || sum <= 0) {
      // Every arc is at the minimum: share the circle equally.
      return values.map(value => (value > 0 ? circumference / shown.length : 0))
    }
    const under = free.filter(i => (values[i] / sum) * rest < minLength)
    if (!under.length) return values.map((value, i) => (value <= 0 ? 0 : raised.has(i) ? minLength : (value / sum) * rest))
    under.forEach(i => raised.add(i))
  }
}

/** `part` as a percentage of `whole`; a part above zero too small to show says "<" the smallest step instead of 0%. */
export function shareLabel(part: number, whole: number, percent: (ratio: number) => string) {
  if (!(whole > 0)) return undefined
  const ratio = part / whole
  return ratio > 0 && ratio < 0.001 ? `<${percent(0.001)}` : percent(ratio)
}

export type Point = { x: number; y: number }
export type PlotBox = {
  width: number
  height: number
  xMax: number
  /** The y range is at least this wide, centred on the data, so a change far smaller than the values draws nearly flat. */
  minYSpan?: number
}

const round = (n: number) => Math.round(n * 10) / 10

/** SVG paths of a line and the area under it, and where the last point sits; x from 0 to `xMax`, y fitted to the data (at least `minYSpan` wide). */
export function linePaths(points: Point[], { width, height, xMax, minYSpan = 0 }: PlotBox) {
  if (!points.length) return { line: '', area: '', end: null }
  const ys = points.map(p => p.y)
  const [low, high] = [Math.min(...ys), Math.max(...ys)]
  const pad = Math.max(0, minYSpan - (high - low)) / 2
  const [min, max] = [low - pad, high + pad]
  const sy = (y: number) => (max === min ? height / 2 : height - ((y - min) / (max - min)) * height)
  const sx = (x: number) => (xMax > 0 ? (x / xMax) * width : 0)
  const coords = points.map(p => `${round(sx(p.x))} ${round(sy(p.y))}`)
  const line = `M${coords.join(' L')}`
  const [first, last] = [points[0], points[points.length - 1]]
  const end = { x: round(sx(last.x)), y: round(sy(last.y)) }
  return { line, area: `${line} L${end.x} ${height} L${round(sx(first.x))} ${height} Z`, end }
}
