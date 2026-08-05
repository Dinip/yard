import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { toast } from "sonner";
import { DeviceConsole } from "@/components/device-console";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { trpc } from "@/lib/trpc";
import { relativeTime } from "@/lib/utils";

export const Route = createFileRoute("/_app/devices/$deviceId")({
  loader: ({ context, params }) =>
    context.queryClient.ensureQueryData(
      context.trpc.device.get.queryOptions({ id: params.deviceId }),
    ),
  component: DevicePage,
});

function DevicePage() {
  const { deviceId } = Route.useParams();
  const qc = useQueryClient();
  const { data: device } = useQuery(trpc.device.get.queryOptions({ id: deviceId }));
  const { data: me } = useQuery(trpc.user.me.queryOptions());

  const invalidate = () => {
    qc.invalidateQueries({ queryKey: trpc.device.get.queryKey({ id: deviceId }) });
    qc.invalidateQueries({ queryKey: trpc.device.list.queryKey() });
  };

  const reserve = useMutation(
    trpc.device.reserve.mutationOptions({
      onSuccess: () => {
        toast.success("Device reserved");
        invalidate();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  const release = useMutation(
    trpc.device.release.mutationOptions({
      onSuccess: () => {
        toast.success("Device released");
        invalidate();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  if (!device) return null;
  const mine = device.reservation?.userId === me?.id;

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center gap-3">
        <Button variant="ghost" size="icon" asChild>
          <Link to="/devices">
            <ArrowLeft className="size-4" />
          </Link>
        </Button>
        <div>
          <h1 className="font-semibold text-2xl">{device.name ?? device.id}</h1>
          <p className="text-muted-foreground text-sm">
            {device.platform} {device.osVersion} · {device.model} · {device.provider.name}
          </p>
        </div>
        <Badge variant="outline" className="ml-2">
          {device.status}
        </Badge>
        <div className="flex-1" />
        {device.reservation ? (
          mine || me?.isAdmin ? (
            <Button
              variant="outline"
              disabled={release.isPending}
              onClick={() => release.mutate({ deviceId })}
            >
              Release
            </Button>
          ) : (
            <span className="text-muted-foreground text-sm">
              Held by {device.reservation.ownerName}
            </span>
          )
        ) : (
          <Button disabled={reserve.isPending} onClick={() => reserve.mutate({ deviceId })}>
            Reserve
          </Button>
        )}
      </div>

      <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_320px]">
        <Card className="min-h-[480px]">
          <CardContent className="flex h-full min-h-0 items-center justify-center text-center text-muted-foreground text-sm">
            {mine ? (
              <DeviceConsole deviceId={deviceId} active className="h-[70svh] w-full" />
            ) : (
              <p>Reserve this device to open a session.</p>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-base">Details</CardTitle>
          </CardHeader>
          <CardContent className="grid gap-2 text-sm">
            <Detail label="Identifier" value={device.id} mono />
            <Detail label="Manufacturer" value={device.manufacturer} />
            <Detail
              label="Display"
              value={
                device.displayWidth
                  ? `${device.displayWidth}×${device.displayHeight}${device.displayScale ? ` @${device.displayScale}x` : ""}`
                  : null
              }
            />
            <Detail label="ABI" value={device.abi} />
            <Detail label="SDK" value={device.sdk?.toString()} />
            <Detail label="Codec" value={device.streamCodec} mono />
            <Detail label="Seen" value={relativeTime(device.presentAt)} />
            {device.reservation && (
              <Detail
                label="Reserved until"
                value={new Date(device.reservation.expiresAt).toLocaleTimeString()}
              />
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}

function Detail({ label, value, mono }: { label: string; value?: string | null; mono?: boolean }) {
  if (!value) return null;
  return (
    <div className="flex justify-between gap-4">
      <span className="text-muted-foreground">{label}</span>
      <span className={`truncate ${mono ? "font-mono text-xs" : ""}`}>{value}</span>
    </div>
  );
}
