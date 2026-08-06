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
import { useReservationRenewal } from "@/hooks/use-reservation-renewal";
import { trpc } from "@/lib/trpc";

/**
 * Holds a reservation open for as long as the window is, and asks before the
 * idle policy takes it away.
 *
 * A component rather than a hook call in each route because the two are one
 * feature: the renewal is what stops the device being reclaimed, and the dialog
 * is the only warning a user gets that it is about to be. Both the device page
 * and the popout render this, so whichever window is in front keeps the device.
 */
export function ReservationKeeper({
  reservationId,
  expiresAt,
  lastActivityAt,
}: {
  reservationId: string | undefined;
  expiresAt: string | Date | undefined;
  lastActivityAt: string | Date | undefined;
}) {
  // Every signed-in user may read the policy their own session is governed by.
  const { data: policy } = useQuery(trpc.settings.public.queryOptions());

  const { idleRemainingMs, warning, keepAlive } = useReservationRenewal(reservationId, expiresAt, {
    lastActivityAt,
    timeoutSeconds: policy?.idleTimeoutSeconds,
  });

  // Dismissing is per warning window: interacting resets `warning` to false,
  // and the next time it goes true the dialog is due again.
  const [dismissed, setDismissed] = useState(false);
  useEffect(() => {
    if (!warning) setDismissed(false);
  }, [warning]);

  const open = warning && !dismissed && idleRemainingMs !== null;

  return (
    <Dialog open={open} onOpenChange={(next) => !next && setDismissed(true)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Still using this device?</DialogTitle>
          <DialogDescription>
            Nobody has touched it for a while. It will be released in{" "}
            <span className="font-mono">{formatCountdown(idleRemainingMs ?? 0)}</span> and become
            available to everyone else.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="ghost" onClick={() => setDismissed(true)}>
            Let it go
          </Button>
          <Button
            onClick={() => {
              keepAlive();
              setDismissed(true);
            }}
          >
            Keep it
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** `m:ss`, clamped at zero — a negative countdown reads as a bug. */
export function formatCountdown(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}
