import { useMutation } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import { trpc } from "@/lib/trpc";

/**
 * Renew every third of the reservation's lifetime.
 *
 * Two renewals may fail — a lost network, a coordinator restart — before the
 * reaper takes the device, which is what makes this a heartbeat rather than a
 * deadline race.
 */
const RENEW_FRACTION = 3;

/** Floor, so a short TTL cannot turn this into a request storm. */
const MIN_INTERVAL = 30_000;

/** Interactions are coalesced to this, so a drag is not a render per frame. */
const INTERACTION_RESOLUTION = 1_000;

/** Warn once this little of the idle budget is left. */
const WARN_FRACTION = 0.1;

/** What the browser counts as someone using the tab. */
const INTERACTION_EVENTS = ["pointerdown", "keydown", "paste", "wheel"] as const;

export interface ReservationRenewal {
  /** Renewals are failing — the reservation may already be gone. */
  failed: boolean;
  /**
   * Milliseconds until the idle timeout releases the device, or `null` when no
   * idle policy is configured.
   */
  idleRemainingMs: number | null;
  /** Whether the idle deadline is close enough to ask the user about it. */
  warning: boolean;
  /** "Keep it" — pushes the deadline out without touching the device. */
  keepAlive: () => void;
}

/**
 * Keep a held reservation alive while the page is open, and watch its idle
 * deadline.
 *
 * The reaper releases lapsed and idle reservations, and this is the other half
 * of both. Renewal is a heartbeat: a tab with a live session says so, and a tab
 * that closed stops saying it.
 *
 * Idle activity is a different question from renewal, and the two are
 * deliberately not merged. **The provider is authoritative** — it sees input on
 * the session plane and adb traffic the browser cannot see at all — and what is
 * sent from here is a *floor* under that: a user reading a crash log on a device
 * they reserved is still using it, even with no frames flowing and nothing
 * reaching the device. The coordinator takes the later of the two, so neither
 * source can wind the other's clock back.
 */
export function useReservationRenewal(
  reservationId: string | undefined,
  expiresAt: string | Date | undefined,
  idle?: {
    /** Server's view of last use, the floor this hook raises. */
    lastActivityAt: string | Date | undefined;
    /** `null` or undefined means the policy is off. */
    timeoutSeconds: number | null | undefined;
  },
): ReservationRenewal {
  const renew = useMutation(trpc.device.renew.mutationOptions());
  const { mutate } = renew;

  const [interactedAt, setInteractedAt] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());

  const timeoutSeconds = idle?.timeoutSeconds ?? null;
  const timeoutMs = timeoutSeconds === null ? null : timeoutSeconds * 1000;

  // Renewal carries the interaction timestamp, so the server hears about tab
  // use it has no other way to see.
  const interactionRef = useRef<number | null>(null);
  interactionRef.current = interactedAt;

  const renewNow = useCallback(() => {
    if (!reservationId) return;
    mutate({ reservationId, interactedAt: interactionRef.current ?? undefined });
  }, [mutate, reservationId]);

  useEffect(() => {
    if (!reservationId || !expiresAt) return;

    // Derive the cadence from the actual lifetime rather than assuming the
    // server's TTL: the two must not be able to drift apart.
    const lifetime = new Date(expiresAt).getTime() - Date.now();
    const interval = Math.max(MIN_INTERVAL, lifetime / RENEW_FRACTION);

    const timer = setInterval(renewNow, interval);
    return () => clearInterval(timer);
  }, [reservationId, expiresAt, renewNow]);

  // Note interaction whatever it lands on: the canvas, a toolbar button, the
  // details card. Capture phase, because a handler that stops propagation is
  // still someone using the tab.
  useEffect(() => {
    if (!reservationId) return;

    const note = () => {
      const at = Date.now();
      setInteractedAt((previous) =>
        previous !== null && at - previous < INTERACTION_RESOLUTION ? previous : at,
      );
    };

    for (const event of INTERACTION_EVENTS) {
      window.addEventListener(event, note, { capture: true, passive: true });
    }
    return () => {
      for (const event of INTERACTION_EVENTS) {
        window.removeEventListener(event, note, { capture: true });
      }
    };
  }, [reservationId]);

  // A one-second tick, but only while there is a countdown to render.
  useEffect(() => {
    if (!reservationId || timeoutMs === null) return;
    const timer = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(timer);
  }, [reservationId, timeoutMs]);

  const lastActivity = Math.max(
    idle?.lastActivityAt ? new Date(idle.lastActivityAt).getTime() : 0,
    interactedAt ?? 0,
  );
  const idleRemainingMs =
    timeoutMs === null || lastActivity === 0 ? null : lastActivity + timeoutMs - now;

  // Interacting inside the warning band has to reach the *server*, not just
  // this hook's arithmetic: the reaper reads the database, and the scheduled
  // renewal can be minutes away on a TTL far longer than the idle timeout.
  const lastPushRef = useRef(0);
  useEffect(() => {
    if (interactedAt === null || timeoutMs === null) return;
    if (interactedAt - lastPushRef.current < timeoutMs / RENEW_FRACTION) return;
    lastPushRef.current = interactedAt;
    renewNow();
  }, [interactedAt, timeoutMs, renewNow]);

  return {
    failed: renew.isError,
    idleRemainingMs,
    warning:
      idleRemainingMs !== null && timeoutMs !== null && idleRemainingMs < timeoutMs * WARN_FRACTION,
    keepAlive: () => {
      setInteractedAt(Date.now());
      lastPushRef.current = Date.now();
      if (reservationId) mutate({ reservationId, interactedAt: Date.now() });
    },
  };
}
