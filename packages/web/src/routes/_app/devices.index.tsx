import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Search } from "lucide-react";
import { useMemo, useState } from "react";
import { DeviceCard } from "@/components/device-card";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { useDeviceStream } from "@/hooks/use-device-stream";
import { trpc } from "@/lib/trpc";
import { platformLabel } from "@/lib/utils";

export const Route = createFileRoute("/_app/devices/")({
  loader: ({ context }) =>
    context.queryClient.ensureQueryData(context.trpc.device.list.queryOptions()),
  component: DevicesPage,
});

const PLATFORMS = ["all", "ios", "android"] as const;

function DevicesPage() {
  const { connected, pollInterval } = useDeviceStream();
  const { data: devices, isLoading } = useQuery({
    ...trpc.device.list.queryOptions(),
    refetchInterval: pollInterval,
  });
  const { data: me } = useQuery(trpc.user.me.queryOptions());
  const [q, setQ] = useState("");
  const [platform, setPlatform] = useState<(typeof PLATFORMS)[number]>("all");

  const filtered = useMemo(() => {
    const needle = q.trim().toLowerCase();
    const matches = (devices ?? []).filter((d) => {
      if (platform !== "all" && d.platform !== platform) return false;
      if (!needle) return true;
      return [d.name, d.model, d.id, d.osVersion, d.manufacturer]
        .filter(Boolean)
        .some((v) => String(v).toLowerCase().includes(needle));
    });
    // The devices you are holding sort to the front: they are the ones you came
    // back to the list for, and a farm is big enough that scanning for them is
    // the common case.
    return matches.sort(
      (a, b) => Number(b.reservation?.userId === me?.id) - Number(a.reservation?.userId === me?.id),
    );
  }, [devices, q, platform, me?.id]);

  const mineCount = useMemo(
    () => (devices ?? []).filter((d) => d.reservation?.userId === me?.id).length,
    [devices, me?.id],
  );

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center gap-3">
        {/* Centred, and `leading-none` on every item so that means something:
            each line box otherwise carries its own leading — 8px of it on the
            2xl heading — so centring the *boxes* leaves the small text sitting
            low against the heading's glyphs. Stripped to the font box, the
            three optical centres line up. */}
        <div className="flex items-center gap-3">
          <h1 className="font-semibold text-2xl leading-none">Devices</h1>
          <span className="text-muted-foreground text-sm leading-none">
            {filtered.length} of {devices?.length ?? 0}
          </span>
          {mineCount > 0 && (
            <span className="rounded-full bg-primary/15 px-2 py-0.5 font-medium text-primary text-xs leading-none">
              {mineCount} yours
            </span>
          )}
          <LiveIndicator connected={connected} />
        </div>
        <div className="flex-1" />
        <div className="flex rounded-md border p-0.5">
          {PLATFORMS.map((p) => (
            <button
              key={p}
              type="button"
              onClick={() => setPlatform(p)}
              className={`rounded px-3 py-1 text-sm transition-colors ${
                platform === p ? "bg-accent text-foreground" : "text-muted-foreground"
              }`}
            >
              {p === "all" ? "All" : platformLabel(p)}
            </button>
          ))}
        </div>
        <div className="relative w-64">
          <Search className="-translate-y-1/2 absolute top-1/2 left-2.5 size-4 text-muted-foreground" />
          <Input
            className="pl-8"
            placeholder="Filter by name, model, udid…"
            value={q}
            onChange={(e) => setQ(e.target.value)}
          />
        </div>
      </div>

      {isLoading ? (
        <div className="grid grid-cols-[repeat(auto-fill,minmax(260px,1fr))] gap-4">
          {[0, 1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-40" />
          ))}
        </div>
      ) : filtered.length === 0 ? (
        <EmptyState hasAny={(devices?.length ?? 0) > 0} />
      ) : (
        <div className="grid grid-cols-[repeat(auto-fill,minmax(260px,1fr))] gap-4">
          {filtered.map((d) => (
            <DeviceCard
              key={d.id}
              device={d}
              mine={Boolean(me) && d.reservation?.userId === me?.id}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function EmptyState({ hasAny }: { hasAny: boolean }) {
  return (
    <div className="flex flex-col items-center gap-2 rounded-lg border border-dashed py-20 text-center">
      <p className="font-medium">{hasAny ? "No devices match the filter" : "No devices yet"}</p>
      <p className="max-w-md text-muted-foreground text-sm">
        {hasAny
          ? "Try clearing the search or platform filter."
          : "Devices appear here once a provider connects and reports its inventory. Create a provider token under Providers to get started."}
      </p>
    </div>
  );
}

/** Distinguishes "nothing is changing" from "we stopped hearing about changes". */
function LiveIndicator({ connected }: { connected: boolean }) {
  return (
    <span
      className="flex items-center gap-1.5 text-muted-foreground text-xs leading-none"
      title={connected ? "Receiving live updates" : "Live stream unavailable — polling every 5s"}
    >
      <span
        className={`size-1.5 rounded-full ${connected ? "animate-pulse bg-success" : "bg-muted-foreground"}`}
      />
      {connected ? "live" : "polling"}
    </span>
  );
}
