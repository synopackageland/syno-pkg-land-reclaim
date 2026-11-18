/** Retry only DSM/transport readiness, never a business mutation or product assertion. */
export async function dsmReady<T>(read: () => Promise<T>, deadlineMs = 10 * 60_000): Promise<T> {
  const until = Date.now() + deadlineMs;
  for (;;) {
    try { return await read(); }
    catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (Date.now() >= until || !/timeout|net::|network|desktop|ERR_ABORTED|ECONNRESET|ECONNREFUSED/i.test(message)) throw error;
      console.log('Waiting for DSM network/desktop readiness');
      await new Promise(resolve => setTimeout(resolve, 2000));
    }
  }
}
