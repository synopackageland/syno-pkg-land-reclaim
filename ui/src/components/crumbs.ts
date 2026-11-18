// Explore breadcrumbs: the crumb list is a pure function of the path
// the page shows, so a superseded navigation is replaced instead of appended.
import type { Path } from '@/types'

/** The UTF-8 bytes of `text` as the lowercase hex the CLI prints in `path_hex`. */
function encodeHex(text: string): string {
  return Array.from(new TextEncoder().encode(text), byte => byte.toString(16).padStart(2, '0')).join('')
}

/** The last path segment, or the whole path for a root. */
export function crumbName(path: string): string {
  return path.replace(/\/$/, '').split('/').at(-1) || path
}

const trimTrailingSlashes = (path: string) => path.replace(/\/+$/, '')

/**
 * The crumbs from `mount` down to `target`: the volume, then one crumb per path
 * segment. Either path can be unknown while the page has no volume yet.
 */
export function crumbChain(mount: Path | null, target: Path | null): Path[] {
  if (!mount) return target ? [target] : []
  const root = trimTrailingSlashes(mount.path)
  if (!target) return [mount]
  const full = trimTrailingSlashes(target.path)
  if (full === root) return [mount]
  // A target outside the volume has no chain here; the volume's root is the safe crumb.
  if (!full.startsWith(`${root}/`)) return [mount]
  const names = full.slice(root.length + 1).split('/')
  const ancestors = ancestorHex(root, target, names.length)
  const crumbs: Path[] = [mount]
  let path = root
  names.forEach((name, index) => {
    path += `/${name}`
    const path_hex = ancestors?.[index]
    crumbs.push(path_hex ? { path, path_hex } : { path })
  })
  return crumbs
}

/**
 * The full `path_hex` of each ancestor of `target`, so a crumb still names the
 * exact bytes when a folder is not UTF-8. `/` is `2f` in UTF-8 and no other byte
 * contains it, so splitting the hex there matches the text segments. The CLI's
 * mount paths are ASCII, so the mount's hex is a literal prefix; anything else
 * falls back to lossy text.
 */
function ancestorHex(root: string, target: Path, count: number): string[] | null {
  if (!target.path_hex) return null
  const prefix = `${encodeHex(root)}2f`
  if (!target.path_hex.startsWith(prefix)) return null
  const segments = target.path_hex.slice(prefix.length).split('2f')
  if (segments.length !== count) return null
  const hexes: string[] = []
  let hex = prefix.slice(0, -2)
  for (const segment of segments) {
    hex += `2f${segment}`
    hexes.push(hex)
  }
  return hexes
}
