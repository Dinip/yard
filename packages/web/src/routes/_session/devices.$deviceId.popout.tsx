import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useEffect } from "react";
import { DeviceConsole } from "@/components/device-console";
import { usePopoutHeartbeat } from "@/hooks/use-popout-presence";
import { useReservationRenewal } from "@/hooks/use-reservation-renewal";
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

  // Whichever window is streaming keeps the reservation. Phase 6 deliberately
  // did the opposite — a popout left open should not hold a device nobody is
  // watching — but that guard cost a user who closed the parent tab their
  // device mid-session, and phase 10's idle timeout is the real backstop for
  // the case it was written for.
  useReservationRenewal(
    mine ? device?.reservation?.id : undefined,
    mine ? device?.reservation?.expiresAt : undefined,
  );

  // Tells the parent tab to stand down, and closes this window when it asks
  // for the stream back.
  usePopoutHeartbeat(deviceId, Boolean(mine));

  if (!device) return null;

  return (
    <div className="flex h-svh flex-col bg-background">
      {mine ? (
        <DeviceConsole
          deviceId={deviceId}
          active
          className="flex-1"
          showPopout={false}
          controls="overlay"
        />
      ) : (
        <div className="flex flex-1 items-center justify-center text-center text-muted-foreground text-sm">
          <p>You do not hold this device. Reserve it in the main window and reopen this one.</p>
        </div>
      )}
    </div>
  );
}
