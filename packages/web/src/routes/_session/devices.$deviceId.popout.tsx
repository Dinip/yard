import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useEffect } from "react";
import { DeviceConsole } from "@/components/device-console";
import { trpc } from "@/lib/trpc";

/**
 * Chrome-free single-device window.
 *
 * It joins the *same* reservation as the tab that opened it, so it neither
 * reserves nor releases — closing it must not take the device away from the
 * page that owns it.
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

  if (!device) return null;
  // No renewal here: the popout shares the parent tab's reservation, and a
  // popout left open should not keep a device that nobody is watching.
  const mine = device.reservation?.userId === me?.id;

  return (
    <div className="flex h-svh flex-col gap-2 bg-background p-2">
      {mine ? (
        <DeviceConsole deviceId={deviceId} active className="flex-1" showPopout={false} />
      ) : (
        <div className="flex flex-1 items-center justify-center text-center text-muted-foreground text-sm">
          <p>You do not hold this device. Reserve it in the main window and reopen this one.</p>
        </div>
      )}
    </div>
  );
}
