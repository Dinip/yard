import { useMutation } from "@tanstack/react-query";
import { useEffect } from "react";
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

/**
 * Keep a held reservation alive while the page is open.
 *
 * The reaper releases lapsed reservations, and this is the other half of that:
 * a tab with a live session says so, and a tab that closed stops saying it. A
 * device is then freed a TTL after the user actually walked away, rather than
 * never.
 *
 * Deliberately not tied to the session socket. A user reading a crash log on a
 * device they reserved is still using it, even with no frames flowing.
 */
export function useReservationRenewal(
  reservationId: string | undefined,
  expiresAt: string | undefined,
) {
  const renew = useMutation(trpc.device.renew.mutationOptions());
  const { mutate } = renew;

  useEffect(() => {
    if (!reservationId || !expiresAt) return;

    // Derive the cadence from the actual lifetime rather than assuming the
    // server's TTL: the two must not be able to drift apart.
    const lifetime = new Date(expiresAt).getTime() - Date.now();
    const interval = Math.max(MIN_INTERVAL, lifetime / RENEW_FRACTION);

    const timer = setInterval(() => mutate({ reservationId }), interval);
    return () => clearInterval(timer);
  }, [reservationId, expiresAt, mutate]);

  return { failed: renew.isError };
}
