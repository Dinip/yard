import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { ArrowLeft, ExternalLink } from "lucide-react";
import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { DeviceConsole } from "@/components/device-console";
import {
  Countdown,
  ReservationKeeper,
  useReservationKeeper,
} from "@/components/reservation-keeper";
import { SessionEndedDialog } from "@/components/session-ended-dialog";
import { SidePanel } from "@/components/side-panel";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
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
import { useSessionEnded } from "@/hooks/use-session-ended";
import { openPopout } from "@/lib/popout";
import { loadPanelOpen, savePanelOpen } from "@/lib/side-panels";
import { trpc } from "@/lib/trpc";
import { platformLabel, relativeTime } from "@/lib/utils";

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
  /** Openly joined this session: full control, no reservation. */
  const observing = Boolean(
    me && device?.reservation?.observers.some((o) => o.userId === me.id) && !mine,
  );
  /** Whether there is a live session here — what the rail and screen need. */
  const inSession = mine || observing;

  // Only worth asking about while somebody else has it and we are not already
  // in. The device stream invalidates this along with everything else.
  const canAsk = Boolean(device?.reservation) && !mine && !observing;
  const { data: myRequest } = useQuery({
    ...trpc.device.myJoinRequest.queryOptions({ deviceId }),
    enabled: canAsk,
    refetchInterval: pollInterval,
  });
  const awaitingAnswer = canAsk && myRequest?.state === "pending";

  // A popout takes the stream; this tab keeps the reservation and the page.
  const { poppedOut, reclaim } = usePopoutPresence(deviceId);

  const invalidate = useCallback(() => {
    qc.invalidateQueries({ queryKey: trpc.device.get.queryKey({ id: deviceId }) });
    qc.invalidateQueries({ queryKey: trpc.device.list.queryKey() });
    qc.invalidateQueries({ queryKey: trpc.device.myJoinRequest.queryKey({ deviceId }) });
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

  const { ended, reportEnded } = useSessionEnded(inSession, releasedHere);
  const onRevoked = useCallback(
    (reason?: string) => {
      // The header still offered "Release" for a device the user no longer has.
      invalidate();
      reportEnded(reason);
    },
    [invalidate, reportEnded],
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
    trpc.device.leaveSession.mutationOptions({
      onSuccess: invalidate,
      onError: (e) => toast.error(e.message),
    }),
  );

  const requestJoin = useMutation(
    trpc.device.requestJoin.mutationOptions({
      onSuccess: () => {
        toast.success("Asked to join. Waiting for an answer.");
        invalidate();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  const cancelJoinRequest = useMutation(
    trpc.device.cancelJoinRequest.mutationOptions({
      onSuccess: invalidate,
      onError: (e) => toast.error(e.message),
    }),
  );

  const answerJoinRequest = useMutation(
    trpc.device.answerJoinRequest.mutationOptions({
      onSuccess: invalidate,
      onError: (e) => toast.error(e.message),
    }),
  );

  const answerAdbAuth = useMutation(
    trpc.device.answerAdbAuthRequest.mutationOptions({
      onSuccess: (result) => {
        toast.success(
          result.approved
            ? "Key approved and added to your account"
            : "Key denied — the connection was closed",
        );
        invalidate();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  // An approval announces itself — the console simply opens. Being turned down
  // or timing out does not, so say it once, on the transition.
  const lastAnswer = useRef<string | null>(null);
  const requestState = myRequest?.state ?? null;
  useEffect(() => {
    const previous = lastAnswer.current;
    lastAnswer.current = requestState;
    if (previous !== "pending") return;
    if (requestState === "denied") toast.error("Your request to join was declined");
    if (requestState === "expired") toast("Your request to join went unanswered");
  }, [requestState]);

  // Only the holder renews. Another user's tab must not keep a device they do
  // not hold alive — but it still gets `idleDeadline`, which needs no renewal.
  const renewal = useReservationKeeper(device?.reservation, mine);

  const [detailsOpen, setDetailsOpen] = useState(() => loadPanelOpen("details"));
  const toggleDetails = () =>
    setDetailsOpen((open) => {
      savePanelOpen("details", !open);
      return !open;
    });

  if (!device) return null;

  return (
    // Locked to the viewport, so the screen gets every pixel the header and the
    // rail do not: a device is used in portrait almost always, and the old page
    // spent its height on chrome and then scrolled.
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      {mine && <ReservationKeeper renewal={renewal} />}
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

      {/* This one *is* a gate — nobody is in the session until it is answered. */}
      {mine && device.reservation && (
        <JoinRequestPrompt
          requests={device.reservation.joinRequests}
          pending={answerJoinRequest.isPending}
          onAnswer={(requestId, approve) => answerJoinRequest.mutate({ requestId, approve })}
        />
      )}

      {/* Also a gate: the connect sits parked on the developer's terminal
          until this is answered. */}
      {mine && device.reservation && (
        <AdbAuthPrompt
          requests={device.reservation.adbAuthRequests}
          pending={answerAdbAuth.isPending}
          onAnswer={(requestId, approve) => answerAdbAuth.mutate({ requestId, approve })}
        />
      )}

      {/* One line, not two: every row here is a row the screen does not get.
          It wraps rather than overflowing when the join/release cluster is at
          its widest. */}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <Button variant="ghost" size="icon" asChild>
          <Link to="/devices">
            <ArrowLeft className="size-4" />
          </Link>
        </Button>
        <h1 className="font-semibold text-xl">{device.name ?? device.id}</h1>
        <p className="text-muted-foreground text-sm">
          {platformLabel(device.platform)} {device.osVersion} · {device.model} ·{" "}
          {device.provider.name}
        </p>
        <Badge variant="outline">{device.status}</Badge>
        {mine && device.reservation && device.reservation.observers.length > 0 && (
          <Badge variant="outline" className="border-warning/30 bg-warning/15 text-warning">
            {describeObservers(device.reservation.observers)} in this session
          </Badge>
        )}
        {/* A dialog dismissed by accident must not be the only way to answer. */}
        {mine && device.reservation && device.reservation.joinRequests.length > 0 && (
          <Badge variant="outline">{device.reservation.joinRequests.length} waiting to join</Badge>
        )}
        <div className="flex-1" />
        {/* A page action rather than a device one, so it sits with Release
            instead of in the rail: it is about this window, not the phone. */}
        {inSession && !poppedOut && (
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              openPopout(deviceId, {
                width: device.displayWidth,
                height: device.displayHeight,
                scale: device.displayScale,
              })
            }
          >
            <ExternalLink className="size-4" /> Pop out
          </Button>
        )}
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
              {/* Anyone who is in the session can step out of it, however they
                  got there. */}
              {observing && (
                <Button
                  variant="outline"
                  size="sm"
                  disabled={leaveSession.isPending}
                  onClick={() => leaveSession.mutate({ deviceId })}
                >
                  Leave session
                </Button>
              )}
              {/* Taking a device from someone mid-session is disruptive and
                  auditable, so it asks for a reason rather than being a
                  one-click action next to Release. */}
              {/* Joining is the gentler of the two: the holder keeps the
                  device and is told, rather than losing it mid-task. An admin
                  does that directly; everyone else asks. */}
              {!observing &&
                (me?.isAdmin ? (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={joinSession.isPending}
                    onClick={() => joinSession.mutate({ deviceId })}
                  >
                    Join session
                  </Button>
                ) : awaitingAnswer ? (
                  <div className="flex items-center gap-2">
                    <span className="text-muted-foreground text-sm">
                      Waiting for {device.reservation.ownerName}…
                    </span>
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={cancelJoinRequest.isPending}
                      onClick={() => cancelJoinRequest.mutate({ deviceId })}
                    >
                      Cancel
                    </Button>
                  </div>
                ) : (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={requestJoin.isPending}
                    onClick={() => requestJoin.mutate({ deviceId })}
                  >
                    Ask to join
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
          <Button
            disabled={reserve.isPending || device.status === "cleaning"}
            onClick={() => reserve.mutate({ deviceId })}
          >
            {device.status === "cleaning" ? "Being cleaned" : "Reserve"}
          </Button>
        )}
      </div>

      <div className="flex min-h-0 flex-1 gap-3">
        {/* No card around the screen: its border and padding were a frame
            around the one thing on the page that wants the room. */}
        {!inSession ? (
          <div className="flex flex-1 items-center justify-center rounded-lg border border-dashed text-center text-muted-foreground text-sm">
            {device.reservation ? (
              <p>
                {device.reservation.ownerName} is using this device.{" "}
                {awaitingAnswer
                  ? "Your request to join is waiting for an answer."
                  : "Ask to join, or wait for it to come free."}
              </p>
            ) : device.status === "cleaning" ? (
              <p>
                This device is being reset after its last session. It will be available in a moment.
              </p>
            ) : (
              <p>Reserve this device to open a session.</p>
            )}
          </div>
        ) : poppedOut ? (
          // Two decoders on one device is waste the user never asked for,
          // so this tab stands down while the popout has the stream.
          <div className="flex flex-1 flex-col items-center justify-center gap-3 rounded-lg border border-dashed text-center text-muted-foreground text-sm">
            <p>This device is open in a popout window.</p>
            <Button variant="outline" size="sm" onClick={reclaim}>
              Bring it back here
            </Button>
          </div>
        ) : (
          <DeviceConsole
            deviceId={deviceId}
            platform={device.platform}
            active
            className="min-w-0 flex-1"
            onRevoked={onRevoked}
          />
        )}

        <SidePanel
          side="right"
          open={detailsOpen}
          onToggle={toggleDetails}
          title="Details"
          width="w-[320px]"
          className="rounded-lg border"
        >
          <div className="grid gap-2 p-3 text-sm">
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
            {policy?.idleTimeoutSeconds != null && renewal.idleDeadline !== null && (
              <Detail
                label="Idle timeout"
                value={
                  <>
                    {Math.round(policy.idleTimeoutSeconds / 60)} min
                    {/* Only once the deadline is near: a countdown ticking all
                        session long, resetting at every touch, is noise. */}
                    {renewal.nearing && (
                      <>
                        {" · releases in "}
                        <Countdown deadline={renewal.idleDeadline} />
                      </>
                    )}
                  </>
                }
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
          </div>
        </SidePanel>
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

type JoinRequest = { id: string; userId: string; name: string | null; note: string | null };

type AdbAuthRequest = {
  requestId: string;
  fingerprint: string;
  comment: string | null;
  /** Serialised over tRPC, like every other date this page renders. */
  askedAt: string;
  expiresAt: string;
};

/**
 * An `adb connect` carrying a key nobody has registered.
 *
 * Approving adds the key to *your* account, which is the only thing the holder
 * can honestly assert: they know somebody is at the door, not who. Somebody
 * else's key belongs on that person's account, added in their own settings.
 *
 * A dialog, and one request at a time, for the same reason as
 * `JoinRequestPrompt`: the connect is parked until this is answered, and an
 * inline card at the top of the page is easy to miss while the 120-second
 * window runs out. Dismissing it lets the request lapse — there is no badge to
 * fall back to, because the answer is only worth anything inside that window.
 */
function AdbAuthPrompt({
  requests,
  pending,
  onAnswer,
}: {
  requests: AdbAuthRequest[];
  pending: boolean;
  onAnswer: (requestId: string, approve: boolean) => void;
}) {
  const [dismissed, setDismissed] = useState<string[]>([]);
  const next = requests.find((r) => !dismissed.includes(r.requestId));

  return (
    <Dialog
      open={Boolean(next)}
      onOpenChange={(open) => {
        if (!open && next) setDismissed((ids) => [...ids, next.requestId]);
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>An adb key is asking to use this device</DialogTitle>
          <DialogDescription>
            Approve only if this is your own machine. The key is added to your account, so your next
            connect anywhere is silent — and every `adb shell` through it is attributed to you.
          </DialogDescription>
        </DialogHeader>

        <div className="grid gap-2">
          <code className="truncate rounded bg-muted px-2 py-1 font-mono text-xs">
            {next?.fingerprint}
          </code>
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-muted-foreground text-xs">
            {next?.comment && <span>from {next.comment}</span>}
            <div className="flex-1" />
            {/* A request whose window has closed is refused on the provider
                whatever this page shows, so the countdown is not decoration. */}
            {next && (
              <span className="font-mono">
                <Countdown deadline={next.expiresAt} /> left
              </span>
            )}
          </div>
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            disabled={pending}
            onClick={() => next && onAnswer(next.requestId, false)}
          >
            Deny
          </Button>
          <Button disabled={pending} onClick={() => next && onAnswer(next.requestId, true)}>
            Approve, it is mine
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * Asks the holder to answer the oldest outstanding request.
 *
 * Deliberately unlike `ObserverDisclosure`, which announces something already
 * true and can be waved away: nobody is in the session until this is answered,
 * so it has two real buttons and no "Got it". One at a time — a queue of people
 * is rare, and answering them one by one is clearer than a list of button
 * pairs. Closing it falls back to the header badge.
 */
function JoinRequestPrompt({
  requests,
  pending,
  onAnswer,
}: {
  requests: JoinRequest[];
  pending: boolean;
  onAnswer: (requestId: string, approve: boolean) => void;
}) {
  const [dismissed, setDismissed] = useState<string[]>([]);
  const next = requests.find((r) => !dismissed.includes(r.id));

  return (
    <Dialog
      open={Boolean(next)}
      onOpenChange={(open) => {
        if (!open && next) setDismissed((ids) => [...ids, next.id]);
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{next?.name ?? "Someone"} wants to join this session</DialogTitle>
          <DialogDescription>
            {next?.note ? `“${next.note}” — ` : ""}
            If you let them in they can see the screen and control the device, the same as you. The
            device stays yours — this is not a release.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button
            variant="outline"
            disabled={pending}
            onClick={() => next && onAnswer(next.id, false)}
          >
            Decline
          </Button>
          <Button disabled={pending} onClick={() => next && onAnswer(next.id, true)}>
            Let them in
          </Button>
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

function Detail({ label, value, mono }: { label: string; value?: ReactNode; mono?: boolean }) {
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
