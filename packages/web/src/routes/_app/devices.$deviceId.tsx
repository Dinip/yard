import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { DeviceConsole } from "@/components/device-console";
import { formatCountdown, ReservationKeeper } from "@/components/reservation-keeper";
import { SessionEndedDialog } from "@/components/session-ended-dialog";
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
import { useDeviceStream } from "@/hooks/use-device-stream";
import { usePopoutPresence } from "@/hooks/use-popout-presence";
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
  const navigate = useNavigate();
  // Without this the detail page got no live updates at all: a device released
  // from elsewhere, or taken back, left this page showing a stale header.
  const { pollInterval } = useDeviceStream();
  const { data: device } = useQuery({
    ...trpc.device.get.queryOptions({ id: deviceId }),
    refetchInterval: pollInterval,
  });
  const { data: me } = useQuery(trpc.user.me.queryOptions());
  const { data: policy } = useQuery(trpc.settings.public.queryOptions());
  const mine = device?.reservation?.userId === me?.id;
  /** An admin who has openly joined this session: full control, no reservation. */
  const observing = Boolean(
    me && device?.reservation?.observers.some((o) => o.userId === me.id) && !mine,
  );

  // A popout takes the stream; this tab keeps the reservation and the page.
  const { poppedOut, reclaim } = usePopoutPresence(deviceId);

  const invalidate = useCallback(() => {
    qc.invalidateQueries({ queryKey: trpc.device.get.queryKey({ id: deviceId }) });
    qc.invalidateQueries({ queryKey: trpc.device.list.queryKey() });
  }, [qc, deviceId]);

  // The reservation is gone from `device.get` by the time the revocation is
  // explained, so the id has to be kept while it is still there.
  const heldReservation = useRef<string | undefined>(undefined);
  if (device?.reservation?.id) heldReservation.current = device.reservation.id;

  /**
   * Set before the release request goes out, not after it returns.
   *
   * Letting go of a device revokes the session like any other release, so the
   * console reports it exactly as it reports being kicked — and the revoke can
   * arrive over the socket before the mutation resolves. A user who clicked
   * Release does not need to be told their session ended.
   */
  const releasedHere = useRef(false);

  const [ended, setEnded] = useState<{ reason?: string } | null>(null);
  const onRevoked = useCallback(
    (reason?: string) => {
      // The header still offered "Release" for a device the user no longer has.
      invalidate();
      if (releasedHere.current) return;
      setEnded({ reason });
    },
    [invalidate],
  );

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
      onMutate: () => {
        releasedHere.current = true;
      },
      onSuccess: () => {
        toast.success("Device released");
        invalidate();
        // There is nothing left on this page to look at: no session, and a
        // device anyone can now take.
        navigate({ to: "/devices" });
      },
      onError: (e) => {
        releasedHere.current = false;
        toast.error(e.message);
      },
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

  const joinSession = useMutation(
    trpc.admin.joinSession.mutationOptions({
      onSuccess: invalidate,
      onError: (e) => toast.error(e.message),
    }),
  );

  const leaveSession = useMutation(
    trpc.admin.leaveSession.mutationOptions({
      onSuccess: invalidate,
      onError: (e) => toast.error(e.message),
    }),
  );

  if (!device) return null;

  return (
    <div className="flex flex-col gap-6">
      {/* Only the holder renews. Another user's tab must not keep a device
          they do not hold alive. */}
      {mine && (
        <ReservationKeeper
          reservationId={device.reservation?.id}
          expiresAt={device.reservation?.expiresAt}
          lastActivityAt={device.reservation?.lastActivityAt}
        />
      )}
      <SessionEndedDialog
        reservationId={heldReservation.current}
        fallbackReason={ended?.reason}
        open={ended !== null}
        onDismiss={() => navigate({ to: "/devices" })}
      />

      {/* Disclosure, not a gate: the admin is already in the session by the
          time this renders, so blocking the holder would achieve nothing but
          getting in their way. */}
      {mine && device.reservation && (
        <ObserverDisclosure observers={device.reservation.observers} />
      )}

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
        {mine && device.reservation && device.reservation.observers.length > 0 && (
          <Badge variant="outline" className="border-warning/30 bg-warning/15 text-warning">
            {describeObservers(device.reservation.observers)} in this session
          </Badge>
        )}
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
              {/* Joining is the gentler of the two: the holder keeps the
                  device and is told, rather than losing it mid-task. */}
              {me?.isAdmin &&
                (observing ? (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={leaveSession.isPending}
                    onClick={() => leaveSession.mutate({ deviceId })}
                  >
                    Leave session
                  </Button>
                ) : (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={joinSession.isPending}
                    onClick={() => joinSession.mutate({ deviceId })}
                  >
                    Join session
                  </Button>
                ))}
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
            {!mine && !observing ? (
              <p>Reserve this device to open a session.</p>
            ) : observing ? (
              <DeviceConsole
                deviceId={deviceId}
                active
                className="h-[70svh] w-full"
                onRevoked={onRevoked}
              />
            ) : poppedOut ? (
              // Two decoders on one device is waste the user never asked for,
              // so this tab stands down while the popout has the stream.
              <div className="flex flex-col items-center gap-3">
                <p>This device is open in a popout window.</p>
                <Button variant="outline" size="sm" onClick={reclaim}>
                  Bring it back here
                </Button>
              </div>
            ) : (
              <DeviceConsole
                deviceId={deviceId}
                active
                className="h-[70svh] w-full"
                onRevoked={onRevoked}
              />
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
            {/* `serial` and `brand` are the same as the identifier and the
                manufacturer on every device seen so far — `ro.serialno` *is*
                the adb serial, and `ro.product.brand` is usually the vendor
                again. Shown only when a device disagrees, so the card carries
                no row that repeats the one above it. */}
            <Detail label="Brand" value={differing(device.brand, device.manufacturer)} />
            <Detail label="Serial" value={differing(device.serial, device.id)} mono />
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
            <Detail label="SDK" value={device.sdk?.toString()} />
            <Detail label="Security patch" value={device.securityPatch} />
            <Detail label="Codec" value={device.streamCodec} mono />
            <Detail label="Seen" value={relativeTime(device.presentAt)} />
            {device.reservation && (
              <Detail
                label="Reserved until"
                value={new Date(device.reservation.expiresAt).toLocaleTimeString()}
              />
            )}
            {device.reservation && policy?.idleTimeoutSeconds != null && (
              <Detail
                label="Idle timeout"
                value={`${Math.round(policy.idleTimeoutSeconds / 60)} min · releases in ${formatCountdown(
                  new Date(device.reservation.lastActivityAt).getTime() +
                    policy.idleTimeoutSeconds * 1000 -
                    Date.now(),
                )}`}
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

type Observer = { userId: string; name: string | null; joinedAt: string | Date };

/** "Ana Silva", or "Ana Silva and 2 others" — a badge, not a list. */
function describeObservers(observers: Observer[]): string {
  const [first, ...rest] = observers;
  const name = first?.name ?? "An admin";
  if (rest.length === 0) return name;
  return `${name} and ${rest.length} other${rest.length === 1 ? "" : "s"}`;
}

/**
 * Tells the holder, once, that somebody joined.
 *
 * Non-blocking on purpose: an admin who joined is already there and already has
 * control, so a modal the holder must clear would be theatre. The badge in the
 * header is the persistent half — this fires only on the transition from nobody
 * to somebody, so a page refresh does not re-announce a session that has had an
 * observer in it for an hour.
 */
function ObserverDisclosure({ observers }: { observers: Observer[] }) {
  const [open, setOpen] = useState(false);
  const [arrival, setArrival] = useState<Observer | null>(null);

  /**
   * Who has already been announced, in a ref rather than state.
   *
   * The first version tracked this in state with `observers` in the dependency
   * array, which loops the moment that array's identity churns between renders:
   * the effect sets state, the state is a dependency, and round it goes. It
   * only held together because react-query's structural sharing happened to
   * keep the reference stable, which is far too subtle a thing to rest a render
   * loop on.
   */
  const announced = useRef<string[]>([]);
  const latest = useRef(observers);
  latest.current = observers;

  // A sorted id list: a primitive, so the effect re-runs when the *people*
  // change and never merely because a new array arrived saying the same thing.
  const present = observers
    .map((o) => o.userId)
    .sort()
    .join(",");

  useEffect(() => {
    const fresh = latest.current.find((o) => !announced.current.includes(o.userId));
    // Someone leaving makes them announceable again if they come back.
    announced.current = present ? present.split(",") : [];
    if (!fresh) return;
    setArrival(fresh);
    setOpen(true);
  }, [present]);

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{arrival?.name ?? "An admin"} joined this session</DialogTitle>
          <DialogDescription>
            They can see the screen and control the device, the same as you. The device is still
            yours — this is not a release.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button onClick={() => setOpen(false)}>Got it</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
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

/**
 * `value` unless it says the same thing as `against`, in which case nothing.
 *
 * Case-insensitive because the two sources disagree on it — `ro.product.brand`
 * answers `samsung` where `ro.product.manufacturer` answers `Samsung`.
 */
function differing(value: string | null | undefined, against: string | null | undefined) {
  if (!value) return null;
  return value.toLowerCase() === (against ?? "").toLowerCase() ? null : value;
}

function Detail({ label, value, mono }: { label: string; value?: string | null; mono?: boolean }) {
  if (!value) return null;
  return (
    <div className="flex justify-between gap-4">
      <span className="shrink-0 text-muted-foreground">{label}</span>
      {/* `min-w-0` is what makes `truncate` work at all inside a flex row:
          without it the value sets the row's minimum width and pushes itself
          out of the card instead of being cut short. */}
      <span className={`min-w-0 truncate text-right ${mono ? "font-mono text-xs" : ""}`}>
        {value}
      </span>
    </div>
  );
}
