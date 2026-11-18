import { test } from '@e2e-dev/web';
import { signIn, dsmPage } from '@synopackageland/cli/e2e';
import { dsmReady } from './support/readiness.ts';

test.setup('DSM sign-in as admin', { sessions: ['dsm'], timeout: 15 * 60_000 }, async ({ app, session }) => {
  await dsmReady(() => signIn(app, 'admin'));
  dsmPage().setDefaultTimeout(120_000);
  dsmPage().setDefaultNavigationTimeout(300_000);
  await session.save('dsm');
});
