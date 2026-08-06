import { useQueryClient } from "@tanstack/react-query";
import { useSubscription } from "@trpc/tanstack-react-query";
import { useEffect, useState } from "react";
import { trpc } from "@/lib/trpc";

/**
 * Keeps the device list live.
 *
 * The subscription carries only a revision counter; receiving one invalidates
 * the device queries so the UI re-reads a single consistent snapshot. Applying
 * deltas client-side would be one fewer round trip and a permanent source of
 * "the UI disagrees with the database" bugs.
 *
 * Falls back to polling if the stream is not connected, so a proxy that eats
 * SSE degrades to the phase-1 behaviour instead of a frozen page.
 */
export function useDeviceStream() {
  const queryClient = useQueryClient();
  const [connected, setConnected] = useState(false);

  const subscription = useSubscription(
    trpc.stream.devices.subscriptionOptions(undefined, {
      onData: () => {
        queryClient.invalidateQueries({ queryKey: trpc.device.list.queryKey() });
        // `pathKey`, so every open `device.get` refreshes too and not just the
        // list. A detail page is a subscriber like any other — it was missing
        // observers joining, a release from another tab, and its own status.
        queryClient.invalidateQueries({ queryKey: trpc.device.get.pathKey() });
        queryClient.invalidateQueries({ queryKey: trpc.provider.list.queryKey() });
      },
      onError: (err) => {
        console.warn("[stream] device subscription error, falling back to polling:", err);
      },
    }),
  );

  useEffect(() => {
    setConnected(subscription.status === "pending");
  }, [subscription.status]);

  return {
    connected,
    /** Hand to `refetchInterval`: polling only matters when the stream is down. */
    pollInterval: connected ? (false as const) : 5_000,
  };
}
