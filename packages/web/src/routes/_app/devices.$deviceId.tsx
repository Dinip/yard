import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { DeviceConsole } from "@/components/device-console";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useReservationRenewal } from "@/hooks/use-reservation-renewal";
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
  const mine = device?.reservation?.userId === me?.id;

  // Only the holder renews. Another user's tab must not keep a device they do
  // not hold alive.
  useReservationRenewal(
    mine ? device?.reservation?.id : undefined,
    mine ? device?.reservation?.expiresAt : undefined,
  );

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

  const forceRelease = useMutation(
    trpc.admin.forceRelease.mutationOptions({
      onSuccess: () => {
        toast.success("Device taken back");
        invalidate();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  if (!device) return null;

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
          mine ? (
            <Button
              variant="outline"
              disabled={release.isPending}
              onClick={() => release.mutate({ deviceId })}
            >
              Release
            </Button>
          ) : (
            <div className="flex items-center gap-3">
              <span className="text-muted-foreground text-sm">
                Held by {device.reservation.ownerName}
              </span>
              {/* Taking a device from someone mid-session is disruptive and
                  auditable, so it asks for a reason rather than being a
                  one-click action next to Release. */}
              {me?.isAdmin && (
                <ForceReleaseDialog
                  owner={device.reservation.ownerName ?? "someone"}
                  pending={forceRelease.isPending}
                  onConfirm={(reason) => forceRelease.mutate({ deviceId, reason })}
                />
              )}
            </div>
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
            <Detail label="Brand" value={device.brand} />
            <Detail label="Serial" value={device.serial} mono />
            <Detail
              label="Display"
              value={
                device.displayWidth
                  ? `${device.displayWidth}×${device.displayHeight}${device.displayScale ? ` @${device.displayScale}x` : ""}${device.displayRotation ? ` · ${device.displayRotation}°` : ""}`
                  : null
              }
            />
            <Detail
              label="Battery"
              value={
                device.batteryLevel == null
                  ? null
                  : `${Math.round(device.batteryLevel * 100)}%${device.batteryState ? ` · ${device.batteryState}` : ""}`
              }
            />
            <Detail label="ABI" value={device.abi} />
            <Detail label="ABI list" value={device.abiList} />
            <Detail label="SDK" value={device.sdk?.toString()} />
            <Detail label="Build" value={device.buildId} />
            <Detail label="Security patch" value={device.securityPatch} />
            <Detail label="Codec" value={device.streamCodec} mono />
            <Detail label="Seen" value={relativeTime(device.presentAt)} />
            {device.reservation && (
              <Detail
                label="Reserved until"
                value={new Date(device.reservation.expiresAt).toLocaleTimeString()}
              />
            )}

            {/* Remote debugging is the holder's tool, not an admin one: it
                hands out a live adb transport to whoever runs the command. */}
            {mine && device.platform === "android" && (
              <RemoteDebugging
                deviceId={deviceId}
                port={device.adbPort}
                host={hostOf(device.provider.publicBaseUrl)}
                onChanged={invalidate}
              />
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}

/**
 * The provider's host, not its URL: whatever proxy fronts the web origin does
 * not forward a raw adb transport, so what a developer needs is the bare host
 * the provider itself bound the port on.
 */
function hostOf(baseUrl: string): string {
  try {
    return new URL(baseUrl).hostname;
  } catch {
    return baseUrl;
  }
}

/**
 * `adb connect` against a reserved device.
 *
 * The transport has existed since phase 4 and nothing in the UI ever reached
 * it. Exposing it is deliberately explicit — it opens a real adb port on the
 * provider — and it stays open until it is turned off or the reservation ends.
 */
function RemoteDebugging({
  deviceId,
  port,
  host,
  onChanged,
}: {
  deviceId: string;
  port: number | null;
  host: string;
  onChanged: () => void;
}) {
  const expose = useMutation(
    trpc.device.adbExpose.mutationOptions({
      onSuccess: (data) => {
        toast.success(`adb listening on ${data.connectString}`);
        onChanged();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  const unexpose = useMutation(
    trpc.device.adbUnexpose.mutationOptions({
      onSuccess: () => {
        toast.success("Remote debugging disabled");
        onChanged();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  const connectString = port ? `${host}:${port}` : null;

  const copy = async () => {
    if (!connectString) return;
    try {
      await navigator.clipboard.writeText(`adb connect ${connectString}`);
      toast.success("Copied");
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "Could not copy");
    }
  };

  return (
    <div className="mt-2 grid gap-2 border-t pt-3">
      <span className="text-muted-foreground">Remote debugging</span>
      {connectString ? (
        <>
          <button
            type="button"
            onClick={copy}
            title="Copy to clipboard"
            className="truncate rounded bg-muted px-2 py-1 text-left font-mono text-xs hover:bg-muted/70"
          >
            adb connect {connectString}
          </button>
          <Button
            variant="outline"
            size="sm"
            disabled={unexpose.isPending}
            onClick={() => unexpose.mutate({ deviceId })}
          >
            Disable
          </Button>
        </>
      ) : (
        <Button
          variant="outline"
          size="sm"
          disabled={expose.isPending}
          onClick={() => expose.mutate({ deviceId })}
        >
          Enable
        </Button>
      )}
    </div>
  );
}

/**
 * Force-release, with the friction it deserves.
 *
 * The holder's session drops the moment this lands — `session.revoke` is a push
 * — so the reason is what they will be told and what the audit log keeps.
 */
function ForceReleaseDialog({
  owner,
  pending,
  onConfirm,
}: {
  owner: string;
  pending: boolean;
  onConfirm: (reason: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline" size="sm">
          Force release
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Take this device back?</DialogTitle>
          <DialogDescription>
            {owner} is using it right now. Their session ends immediately, and this is recorded in
            the audit log.
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-2">
          <Label htmlFor="force-reason">Reason</Label>
          <Input
            id="force-reason"
            value={reason}
            placeholder="Needed for a release build"
            onChange={(event) => setReason(event.target.value)}
          />
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => setOpen(false)}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            disabled={pending}
            onClick={() => {
              onConfirm(reason.trim() || "force-released by admin");
              setOpen(false);
              setReason("");
            }}
          >
            Take it back
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
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
