import type { E2EConfig } from 'e2e';
import { dsmTarget, magpieAgent } from '@synopackageland/cli/e2e';
export default {
  tests: ['tests/e2e/*.e2e.ts'],
  targets: [dsmTarget()],
  agents: { default: magpieAgent() },
  workers: 1,
  retries: 0,
  // Suite setup/teardown also uses this budget: real DSM/SSH and cancellation
  // must finish before a new worker attempts to create the same owned fixture.
  timeout: 10 * 60_000,
} satisfies E2EConfig;
