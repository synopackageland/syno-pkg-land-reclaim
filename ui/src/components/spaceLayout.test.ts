// UI 0.9.7's Space starts a row's items at the start of the row (0.9.6
// centred them, so every row used to say justify-content="start"). A Space
// states justify-content only to place its items elsewhere.
const sources = import.meta.glob('../**/*.vue', { query: '?raw', import: 'default', eager: true }) as Record<string, string>

describe('Space rows', () => {
  it('leave the start of the row unsaid, as the default', () => {
    const restated = Object.entries(sources).flatMap(([file, source]) =>
      [...source.matchAll(/<Space\b[^>]*>/g)]
        .map(([tag]) => tag)
        .filter(tag => /\bjustify-content="(start|flex-start)"/.test(tag))
        .map(tag => `${file}: ${tag}`))
    expect(restated).toEqual([])
  })
})
