import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { trpc } from "@/lib/trpc";

/**
 * Why the session ended, named and blocking.
 *
 * An administrative action and a dropped network were indistinguishable before
 * this: both showed a spinner over the last frame. They are entirely different
 * events to the person they happen to, so this one does not dismiss on a click
 * outside — the device is gone, and continuing to look at a dead console is not
 * a state worth offering.
 *
 * The reason arrives on the wire with the revocation; the *actor* does not, and
 * should not — the provider has no notion of users. So it is read back from the
 * reservation row, which `releaseActive` already populated.
 */
export function SessionEndedDialog({
  reservationId,
  fallbackReason,
  open,
  onDismiss,
  dismissLabel = "Back to devices",
}: {
  reservationId: string | undefined;
  /** What the provider said, shown until (or unless) the row can be read. */
  fallbackReason: string | undefined;
  open: boolean;
  onDismiss: () => void;
  dismissLabel?: string;
}) {
  const { data: outcome } = useQuery({
    ...trpc.device.reservationOutcome.queryOptions({ reservationId: reservationId ?? "" }),
    enabled: open && Boolean(reservationId),
    // The row is written before the revoke is pushed, so one read is enough.
    staleTime: Number.POSITIVE_INFINITY,
    retry: false,
  });

  const reason = outcome?.reason ?? fallbackReason ?? "The reservation ended.";
  const by = outcome?.releasedByName;

  return (
    <Dialog open={open}>
      <DialogContent showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>Session ended</DialogTitle>
          <DialogDescription>
            {by ? (
              <>
                <span className="font-medium text-foreground">{by}</span> ended your session on this
                device.
              </>
            ) : (
              // No actor means the reaper, which is nobody — saying a person
              // did it would be a lie, and "system" tells the user nothing.
              <>Your reservation on this device ended.</>
            )}
          </DialogDescription>
        </DialogHeader>

        <p className="rounded bg-muted px-3 py-2 text-sm">{reason}</p>

        <DialogFooter>
          <Button onClick={onDismiss}>{dismissLabel}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
