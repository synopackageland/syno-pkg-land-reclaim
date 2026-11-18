// The pinned CLI requires backend.methods for a cross-compiled Rust App. Derive
// that build input from the registry; the Rust handler test proves equality.
import {readFileSync, writeFileSync} from 'node:fs';
import {isDeepStrictEqual} from 'node:util';
const source = JSON.parse(readFileSync('operations.json'));
const manifest = JSON.parse(readFileSync('dsm-package.yaml'));
const methods = Object.keys(source.operations).sort();
if (process.argv.includes('--check')) {
  if (!isDeepStrictEqual(methods, manifest.backend.methods)) {
    throw Error('Rust bridge methods drifted; run npm run generate:operations');
  }
} else {
  manifest.backend.methods = methods;
  writeFileSync('dsm-package.yaml', JSON.stringify(manifest,null,2)+'\n');
}
