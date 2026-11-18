// Reuse the locked frontend tooling without adding or changing dependency pins.
import { createRequire } from 'node:module';
const require = createRequire(new URL('../../ui/package.json', import.meta.url));
const ts = require('typescript-eslint');
const globals = require('globals');
export default ts.config(
  ...ts.configs.recommended,
  { languageOptions: { globals: globals.node }, rules: {
    // JSON from the installed CLI/CGI is checked at runtime against its descriptor.
    '@typescript-eslint/no-explicit-any': 'off',
  } },
);
