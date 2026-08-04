import { deviceEvents } from "../../lib/events.ts";
import { protectedProcedure, router } from "../init.ts";

/**
 * Live device-list updates, over tRPC's SSE subscription transport.
 *
 * The subscription yields a **revision counter, not device data**. Clients
 * respond by invalidating their `device.list` query, so the list they render
 * always came from a single consistent read rather than from replaying deltas.
 * That costs one extra round trip per change and removes an entire class of
 * "the UI drifted from the database" bugs.
 */
export const streamRouter = router({
  devices: protectedProcedure.subscription(async function* (opts) {
    let revision = 0;
    let notify: (() => void) | null = null;
    let pendingWake = false;

    const unsubscribe = deviceEvents.subscribe(() => {
      revision += 1;
      if (notify) notify();
      else pendingWake = true;
    });

    try {
      // Emit immediately so a client can tell "connected" from "connecting".
      yield { revision, at: Date.now() };

      while (!opts.signal?.aborted) {
        if (!pendingWake) {
          await new Promise<void>((resolve) => {
            notify = resolve;
            // A client that goes away mid-wait must not keep the generator (and
            // its database connection) alive until the next device event.
            opts.signal?.addEventListener("abort", () => resolve(), { once: true });
          });
          notify = null;
        }
        pendingWake = false;
        if (opts.signal?.aborted) break;
        yield { revision, at: Date.now() };
      }
    } finally {
      unsubscribe();
    }
  }),
});
