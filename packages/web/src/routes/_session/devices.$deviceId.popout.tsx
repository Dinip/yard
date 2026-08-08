import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useCallback, useEffect, useRef, useState } from "react";
import { DeviceConsole } from "@/components/device-console";
import { ReservationKeeper } from "@/components/reservation-keeper";
import { SessionEndedDialog } from "@/components/session-ended-dialog";
import { usePopoutHeartbeat } from "@/hooks/use-popout-presence";
import { trpc } from "@/lib/trpc";

/**
 * Chrome-free single-device window.
 *
 * It joins the *same* reservation as the tab that opened it, so it neither
 * reserves nor releases — closing it must not take the device away from the
 * page that owns it. It does renew, though: see the note below.
 */
export const Route = createFileRoute("/_session/devices/$deviceId/popout")({
  loader: ({ context, params }) =>
    context.queryClient.ensureQueryData(
      context.trpc.device.get.queryOptions({ id: params.deviceId }),
    ),
  component: PopoutPage,
});

function PopoutPage() {
  const { deviceId } = Route.useParams();
  const { data: device } = useQuery(trpc.device.get.queryOptions({ id: deviceId }));
  const { data: me } = useQuery(trpc.user.me.queryOptions());

  useEffect(() => {
    if (device) document.title = device.name ?? device.id;
  }, [device]);

  const mine = device?.reservation?.userId === me?.id;
  /** Joined somebody else's session — an admin, or an approved request. */
  const observing = Boolean(
    me && device?.reservation?.observers.some((o) => o.userId === me.id) && !mine,
  );
  const inSession = mine || observing;

  // Kept while the reservation still exists: it is gone from `device.get` by
  // the time the revocation needs explaining.
  const heldReservation = useRef<string | undefined>(undefined);
  if (device?.reservation?.id) heldReservation.current = device.reservation.id;

  const [ended, setEnded] = useState<{ reason?: string } | null>(null);
  const onRevoked = useCallback((reason?: string) => setEnded({ reason }), []);

  // Tells the parent tab to stand down, and closes this window when it asks
  // for the stream back.
  usePopoutHeartbeat(deviceId, inSession);

  if (!device) return null;

  return (
    <div className="flex h-svh flex-col bg-background">
      {/* This window has nowhere to navigate to, so it offers to close itself.
          A popout left showing a dead console is worse than none. */}
      <SessionEndedDialog
        reservationId={heldReservation.current}
        fallbackReason={ended?.reason}
        open={ended !== null}
        onDismiss={() => window.close()}
        dismissLabel="Close window"
      />
      {/* Whichever window is streaming keeps the reservation. Phase 6
          deliberately did the opposite — a popout left open should not hold a
          device nobody is watching — but that guard cost a user who closed the
          parent tab their device mid-session, and the idle timeout is the real
          backstop for the case it was written for. */}
      {mine && (
        <ReservationKeeper
          reservationId={device.reservation?.id}
          expiresAt={device.reservation?.expiresAt}
          lastActivityAt={device.reservation?.lastActivityAt}
        />
      )}
      {inSession ? (
        <DeviceConsole
          deviceId={deviceId}
          platform={device.platform}
          active
          className="flex-1"
          controls="overlay"
          onRevoked={onRevoked}
        />
      ) : (
        <div className="flex flex-1 items-center justify-center text-center text-muted-foreground text-sm">
          <p>
            You are not in this device's session. Reserve it — or ask to join — in the main window,
            then reopen this one.
          </p>
        </div>
      )}
    </div>
  );
}
