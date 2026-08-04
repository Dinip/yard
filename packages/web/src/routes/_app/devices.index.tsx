import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Search } from "lucide-react";
import { useMemo, useState } from "react";
import { DeviceCard } from "@/components/device-card";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { trpc } from "@/lib/trpc";

export const Route = createFileRoute("/_app/devices/")({
  loader: ({ context }) =>
    context.queryClient.ensureQueryData(context.trpc.device.list.queryOptions()),
  component: DevicesPage,
});

const PLATFORMS = ["all", "ios", "android"] as const;

function DevicesPage() {
  const { data: devices, isLoading } = useQuery({
    ...trpc.device.list.queryOptions(),
    // Until the live `stream` subscription lands (phase 2), poll.
    refetchInterval: 5_000,
  });
  const [q, setQ] = useState("");
  const [platform, setPlatform] = useState<(typeof PLATFORMS)[number]>("all");

  const filtered = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return (devices ?? []).filter((d) => {
      if (platform !== "all" && d.platform !== platform) return false;
      if (!needle) return true;
      return [d.name, d.model, d.id, d.osVersion, d.manufacturer]
        .filter(Boolean)
        .some((v) => String(v).toLowerCase().includes(needle));
    });
  }, [devices, q, platform]);

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="font-semibold text-2xl">Devices</h1>
        <span className="text-muted-foreground text-sm">
          {filtered.length} of {devices?.length ?? 0}
        </span>
        <div className="flex-1" />
        <div className="flex rounded-md border p-0.5">
          {PLATFORMS.map((p) => (
            <button
              key={p}
              type="button"
              onClick={() => setPlatform(p)}
              className={`rounded px-3 py-1 text-sm capitalize transition-colors ${
                platform === p ? "bg-accent text-foreground" : "text-muted-foreground"
              }`}
            >
              {p}
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
            <DeviceCard key={d.id} device={d} />
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
