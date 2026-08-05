import { useCallback, useEffect, useRef, useState } from "react";

/**
 * One live stream per device, across windows.
 *
 * The popout and the tab that opened it each run a full `useDeviceSession` —
 * two sockets, two decoders, two backlogs on one device — because nothing ever
 * told the parent to stand down. A `BroadcastChannel` is enough: both windows
 * are same-origin, and the coordinator has no business knowing how many browser
 * windows a user has open.
 *
 * It is a **heartbeat, not an announcement**. A popout that crashes or is killed
 * never gets to say goodbye, and a parent tab left permanently suspended on a
 * device nobody is watching is worse than a redundant decoder.
 */
type Presence =
  | { kind: "alive" }
  | { kind: "closed" }
  /** The parent wants the stream back; the popout closes itself. */
  | { kind: "reclaim" }
  /** A freshly loaded parent asking whether a popout is already open. */
  | { kind: "who" };

const HEARTBEAT = 2_000;
/** Two missed beats. Short enough to resume promptly, long enough to survive a slow frame. */
const STALE_AFTER = 5_000;

function channelFor(deviceId: string): BroadcastChannel | null {
  if (typeof BroadcastChannel === "undefined") return null;
  return new BroadcastChannel(`farm-device-${deviceId}`);
}

/**
 * Parent side: is a popout live, and a way to take the stream back.
 *
 * `poppedOut` is what suspends the parent's session, so it must never latch on
 * a stale beat — hence the timer, rather than trusting a `closed` message that
 * a crashed window cannot send.
 */
export function usePopoutPresence(deviceId: string): {
  poppedOut: boolean;
  reclaim: () => void;
} {
  const [poppedOut, setPoppedOut] = useState(false);
  const channelRef = useRef<BroadcastChannel | null>(null);

  useEffect(() => {
    const channel = channelFor(deviceId);
    if (!channel) return;
    channelRef.current = channel;

    let lastSeen = 0;
    channel.onmessage = (event: MessageEvent<Presence>) => {
      if (event.data.kind === "alive") {
        lastSeen = Date.now();
        setPoppedOut(true);
      } else if (event.data.kind === "closed") {
        lastSeen = 0;
        setPoppedOut(false);
      }
    };

    // A reloaded parent must re-discover a popout that is already open.
    channel.postMessage({ kind: "who" } satisfies Presence);

    const timer = setInterval(() => {
      if (lastSeen && Date.now() - lastSeen > STALE_AFTER) {
        lastSeen = 0;
        setPoppedOut(false);
      }
    }, HEARTBEAT);

    return () => {
      clearInterval(timer);
      channel.close();
      channelRef.current = null;
    };
  }, [deviceId]);

  const reclaim = useCallback(() => {
    channelRef.current?.postMessage({ kind: "reclaim" } satisfies Presence);
    // Optimistic: the popout's `closed` follows, but the parent should resume
    // the moment the user asks rather than a round-trip later.
    setPoppedOut(false);
  }, []);

  return { poppedOut, reclaim };
}

/** Popout side: announce this window, and close when the parent asks for the stream back. */
export function usePopoutHeartbeat(deviceId: string, active: boolean) {
  useEffect(() => {
    if (!active) return;
    const channel = channelFor(deviceId);
    if (!channel) return;

    const announce = () => channel.postMessage({ kind: "alive" } satisfies Presence);
    announce();
    const timer = setInterval(announce, HEARTBEAT);

    channel.onmessage = (event: MessageEvent<Presence>) => {
      if (event.data.kind === "who") announce();
      if (event.data.kind === "reclaim") window.close();
    };

    // `pagehide` rather than `beforeunload`: it fires on mobile and on
    // bfcache eviction, where `beforeunload` does not.
    const goodbye = () => channel.postMessage({ kind: "closed" } satisfies Presence);
    window.addEventListener("pagehide", goodbye);

    return () => {
      goodbye();
      window.removeEventListener("pagehide", goodbye);
      clearInterval(timer);
      channel.close();
    };
  }, [deviceId, active]);
}
