import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { type ReservationRenewal, useReservationRenewal } from "@/hooks/use-reservation-renewal";
import { trpc } from "@/lib/trpc";

/**
 * Renewal and the idle policy, for a reservation this window holds.
 *
 * Exactly one per window — it drives a renewal timer, and a second instance
 * would double the heartbeat. Whatever else shows the deadline reads
 * `idleDeadline` off the same object.
 */
export function useReservationKeeper(
  reservation:
    | { id: string; expiresAt: string | Date; lastActivityAt: string | Date }
    | null
    | undefined,
  /** False for a device this window does not hold: it must not renew. */
  held: boolean,
): ReservationRenewal {
  // Every signed-in user may read the policy their own session is governed by.
  const { data: policy } = useQuery(trpc.settings.public.queryOptions());

  return useReservationRenewal(held ? reservation?.id : undefined, reservation?.expiresAt, {
    lastActivityAt: reservation?.lastActivityAt,
    timeoutSeconds: policy?.idleTimeoutSeconds,
  });
}

/**
 * Asks before the idle policy takes the device away.
 *
 * The renewal and the dialog are one feature: the renewal is what stops the
 * device being reclaimed, and this is the only warning a user gets that it is
 * about to be. Both the device page and the popout render it, so whichever
 * window is in front keeps the device.
 */
export function ReservationKeeper({ renewal }: { renewal: ReservationRenewal }) {
  const { idleRemainingMs, warning, keepAlive } = renewal;

  // Dismissing is per warning window: interacting resets `warning` to false,
  // and the next time it goes true the dialog is due again.
  const [dismissed, setDismissed] = useState(false);
  useEffect(() => {
    if (!warning) setDismissed(false);
  }, [warning]);

  const open = warning && !dismissed && idleRemainingMs !== null;

  // The reaper sweeps on its own timer, so the deadline passes before the
  // release does. Counting past zero, or still offering "Keep it", claims a
  // certainty the browser does not have — the next sweep may already have taken
  // the device.
  const lapsed = idleRemainingMs !== null && idleRemainingMs <= 0;

  return (
    <Dialog open={open} onOpenChange={(next) => !next && setDismissed(true)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{lapsed ? "Releasing this device" : "Still using this device?"}</DialogTitle>
          <DialogDescription>
            {lapsed ? (
              <>Nobody touched it in time, so it is being released to everyone else.</>
            ) : (
              <>
                Nobody has touched it for a while. It will be released in{" "}
                <span className="font-mono">{formatCountdown(idleRemainingMs ?? 0)}</span> and
                become available to everyone else.
              </>
            )}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="ghost" onClick={() => setDismissed(true)}>
            {lapsed ? "Close" : "Let it go"}
          </Button>
          {!lapsed && (
            <Button
              onClick={() => {
                keepAlive();
                setDismissed(true);
              }}
            >
              Keep it
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * A `m:ss` countdown that ticks on its own — a deadline rendered against an
 * inline `Date.now()` sits still until something else re-renders the page.
 */
export function Countdown({ deadline }: { deadline: number | string | Date }) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(timer);
  }, []);

  return <>{formatCountdown(new Date(deadline).getTime() - now)}</>;
}

/** `m:ss`, clamped at zero — a negative countdown reads as a bug. */
export function formatCountdown(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}
