// Generates src/types/schema/<name>.ts from ../schemas/v1/<name>.schema.json.
// Run `pnpm gen:types` after a schema changes and commit the output.
import { readdir, mkdir, writeFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import { compileFromFile } from 'json-schema-to-typescript'

const schemaDir = fileURLToPath(new URL('../../schemas/v1/', import.meta.url))
const outDir = fileURLToPath(new URL('../src/types/schema/', import.meta.url))

await mkdir(outDir, { recursive: true })
for (const file of (await readdir(schemaDir)).filter(f => f.endsWith('.schema.json')).sort()) {
  const ts = await compileFromFile(schemaDir + file, {
    cwd: schemaDir,
    bannerComment: `// Generated from schemas/v1/${file} by \`pnpm gen:types\`. Do not edit.`,
    additionalProperties: false,
    style: { singleQuote: true, semi: false, printWidth: 120 },
  })
  await writeFile(outDir + file.replace('.schema.json', '.ts'), ts)
}
