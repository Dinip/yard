import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, redirect } from "@tanstack/react-router";
import type { PreloadInfo } from "@yard/protocol";
import { Check, FileArchive, LoaderCircle, Search, Trash2, Upload, X } from "lucide-react";
import { type ReactNode, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { formatBytes } from "@/lib/download";
import { type PreloadGrant, preloadApp } from "@/lib/screen/session";
import { trpc } from "@/lib/trpc";
import type { DeviceListItem } from "@/lib/types";
import { cn, platformLabel } from "@/lib/utils";

export const Route = createFileRoute("/_app/admin/preload")({
  beforeLoad: ({ context }) => {
    if (context.user.role !== "admin") throw redirect({ to: "/devices" });
  },
  component: AdminPreloadPage,
});

type AppPlatform = "ios" | "android";
type DeploymentState = "queued" | "uploading" | "success" | "failed" | "skipped";
type PreloadGrouping = "app" | "device";

type DeploymentResult = {
  state: DeploymentState;
  progress: number;
  message?: string;
};

const DEPLOYABLE_STATUSES = new Set(["ready", "present"]);

function AdminPreloadPage() {
  const queryClient = useQueryClient();
  const fileInput = useRef<HTMLInputElement>(null);
  const [file, setFile] = useState<File | null>(null);
  const [platform, setPlatform] = useState<AppPlatform | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [search, setSearch] = useState("");
  const [dragging, setDragging] = useState(false);
  const [deploying, setDeploying] = useState(false);
  const [preloadGrouping, setPreloadGrouping] = useState<PreloadGrouping>("app");
  const [results, setResults] = useState<Record<string, DeploymentResult>>({});
  const [removeTarget, setRemoveTarget] = useState<{
    deviceId: string;
    appId: string;
    deviceName: string;
  } | null>(null);

  const { data: devices, isLoading } = useQuery({
    ...trpc.device.list.queryOptions(),
    refetchInterval: 5_000,
  });
  const { data: preloads, isLoading: preloadsLoading } = useQuery({
    ...trpc.device.preloads.queryOptions(),
    refetchInterval: 5_000,
  });
  const requestGrants = useMutation(trpc.device.preloadTokens.mutationOptions());
  const removePreload = useMutation(
    trpc.device.removePreload.mutationOptions({
      onSuccess: async () => {
        setRemoveTarget(null);
        toast.success("Preloaded app removed");
        await queryClient.invalidateQueries({ queryKey: trpc.device.preloads.queryKey() });
      },
      onError: (error) => toast.error(error.message),
    }),
  );

  const filteredDevices = useMemo(() => {
    const needle = search.trim().toLowerCase();
    return (devices ?? []).filter((device) => {
      if (!needle) return true;
      return [device.name, device.model, device.id, device.provider.name]
        .filter(Boolean)
        .some((value) => String(value).toLowerCase().includes(needle));
    });
  }, [devices, search]);

  const availableDevices = useMemo(
    () => (devices ?? []).filter((device) => platform && canSelect(device, platform)),
    [devices, platform],
  );

  const selectedCount = useMemo(
    () => (devices ?? []).filter((device) => selected.has(device.id)).length,
    [devices, selected],
  );
  const allAvailableSelected =
    availableDevices.length > 0 && availableDevices.every((device) => selected.has(device.id));

  const deviceById = useMemo(
    () => new Map((devices ?? []).map((device) => [device.id, device])),
    [devices],
  );
  const preloadsByApp = useMemo(
    () => groupPreloadsByApp(preloads ?? [], deviceById),
    [preloads, deviceById],
  );
  const preloadsByDevice = useMemo(
    () => groupPreloadsByDevice(preloads ?? [], deviceById),
    [preloads, deviceById],
  );

  const updateResult = (deviceId: string, result: Partial<DeploymentResult>) => {
    setResults((current) => ({
      ...current,
      [deviceId]: { ...current[deviceId], ...result } as DeploymentResult,
    }));
  };

  const chooseFile = (next: File | undefined) => {
    if (!next) return;
    const nextPlatform = appPlatform(next.name);
    if (!nextPlatform) {
      toast.error("Choose an .apk or .ipa file");
      return;
    }

    setFile(next);
    setPlatform(nextPlatform);
    setResults({});
    setSelected((current) => {
      const compatible = new Set(
        (devices ?? [])
          .filter((device) => device.platform === nextPlatform)
          .map((device) => device.id),
      );
      return new Set([...current].filter((id) => compatible.has(id)));
    });
  };

  const clearFile = () => {
    setFile(null);
    setPlatform(null);
    setSelected(new Set());
    setResults({});
    if (fileInput.current) fileInput.current.value = "";
  };

  const toggleDevice = (device: DeviceListItem) => {
    if (!platform || (!canSelect(device, platform) && !selected.has(device.id))) return;
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(device.id)) next.delete(device.id);
      else next.add(device.id);
      return next;
    });
  };

  const selectAll = () => {
    setSelected((current) => {
      const next = new Set(current);
      if (allAvailableSelected) {
        for (const device of availableDevices) next.delete(device.id);
      } else {
        for (const device of availableDevices) next.add(device.id);
      }
      return next;
    });
  };

  const deploy = async () => {
    if (!file || !platform || selected.size === 0) return;

    const deviceById = new Map((devices ?? []).map((device) => [device.id, device]));
    const deviceIds = [...selected].filter((id) => deviceById.get(id)?.platform === platform);
    if (deviceIds.length === 0) return;

    setDeploying(true);
    setResults(
      Object.fromEntries(deviceIds.map((deviceId) => [deviceId, { state: "queued", progress: 0 }])),
    );

    try {
      const response = await requestGrants.mutateAsync({ deviceIds });
      for (const skipped of response.skipped) {
        updateResult(skipped.deviceId, { state: "skipped", message: skipped.reason });
      }

      let cursor = 0;
      let successCount = 0;
      let failureCount = 0;
      const grants: PreloadGrant[] = response.grants;
      if (grants.length === 0) {
        toast.info("No selected devices were available");
        return;
      }
      const worker = async () => {
        while (cursor < grants.length) {
          const grant = grants[cursor++];
          if (!grant) return;
          updateResult(grant.deviceId, { state: "uploading", progress: 0 });
          try {
            await preloadApp(grant, file, (progress) =>
              updateResult(grant.deviceId, { state: "uploading", progress }),
            );
            successCount += 1;
            updateResult(grant.deviceId, { state: "success", progress: 1 });
          } catch (error) {
            failureCount += 1;
            updateResult(grant.deviceId, {
              state: "failed",
              message: errorMessage(error),
            });
          }
        }
      };

      await Promise.all(Array.from({ length: Math.min(3, grants.length) }, () => worker()));
      await queryClient.invalidateQueries({ queryKey: trpc.device.preloads.queryKey() });

      if (failureCount === 0) {
        toast.success(`${successCount} device${successCount === 1 ? "" : "s"} deployed`);
      } else {
        toast.error(`${failureCount} deployment${failureCount === 1 ? "" : "s"} failed`);
      }
    } catch (error) {
      const message = errorMessage(error);
      setResults((current) =>
        Object.fromEntries(
          Object.entries(current).map(([deviceId, result]) =>
            result.state === "queued"
              ? [deviceId, { state: "failed", progress: 0, message }]
              : [deviceId, result],
          ),
        ),
      );
      toast.error(message);
    } finally {
      setDeploying(false);
    }
  };

  return (
    <div className="flex flex-col gap-6">
      <div>
        <h1 className="font-semibold text-2xl">Preloaded apps</h1>
        <p className="mt-1 text-muted-foreground text-sm">
          Deploy APKs and IPAs across the farm, or remove an existing preload from a device.
        </p>
      </div>

      <Card>
        <CardHeader className="gap-3">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="min-w-0">
              <CardTitle>Installed preloads</CardTitle>
              <CardDescription className="mt-1">
                {(preloads?.length ?? 0).toLocaleString()} installed{" "}
                {countNoun(preloads?.length ?? 0, "copy", "copies")} across{" "}
                {preloadsByApp.length.toLocaleString()}{" "}
                {countNoun(preloadsByApp.length, "app", "apps")} and{" "}
                {preloadsByDevice.length.toLocaleString()}{" "}
                {countNoun(preloadsByDevice.length, "device", "devices")}.
              </CardDescription>
            </div>
            {(preloads?.length ?? 0) > 0 && (
              <div className="flex rounded-md border p-0.5">
                {(["app", "device"] as const).map((grouping) => (
                  <button
                    key={grouping}
                    type="button"
                    aria-pressed={preloadGrouping === grouping}
                    onClick={() => setPreloadGrouping(grouping)}
                    className={cn(
                      "rounded px-3 py-1 text-sm transition-colors",
                      preloadGrouping === grouping
                        ? "bg-accent text-foreground"
                        : "text-muted-foreground hover:text-foreground",
                    )}
                  >
                    By {grouping}
                  </button>
                ))}
              </div>
            )}
          </div>
        </CardHeader>
        <CardContent>
          {preloadsLoading ? (
            <p className="py-6 text-center text-muted-foreground text-sm">Loading preloads…</p>
          ) : preloads?.length === 0 ? (
            <p className="rounded-lg border border-dashed py-8 text-center text-muted-foreground text-sm">
              No apps are preloaded.
            </p>
          ) : (
            <div className="grid gap-2">
              {preloadGrouping === "app"
                ? preloadsByApp.map((group) => (
                    <PreloadGroup
                      key={group.key}
                      title={group.appId}
                      detail={packageSummary(group.preloads)}
                      count={`${group.preloads.length} ${countNoun(group.preloads.length, "device", "devices")}`}
                      platform={group.platform}
                    >
                      {group.preloads.map((preload) => {
                        const target = deviceById.get(preload.deviceId);
                        const deviceName = deviceDisplayName(target, preload.deviceId);
                        return (
                          <PreloadChip
                            key={`${preload.deviceId}:${preload.appId}`}
                            label={deviceName}
                            detail={[
                              target?.provider.name,
                              preload.filename,
                              formatBytes(preload.size),
                            ]
                              .filter(Boolean)
                              .join(" · ")}
                            preload={preload}
                            deviceName={deviceName}
                            target={target}
                            removing={removePreload.isPending}
                            isRemoving={
                              removePreload.isPending &&
                              removePreload.variables?.deviceId === preload.deviceId &&
                              removePreload.variables.appId === preload.appId
                            }
                            onRemove={setRemoveTarget}
                          />
                        );
                      })}
                    </PreloadGroup>
                  ))
                : preloadsByDevice.map((group) => {
                    const target = deviceById.get(group.deviceId);
                    const deviceName = deviceDisplayName(target, group.deviceId);
                    return (
                      <PreloadGroup
                        key={group.deviceId}
                        title={deviceName}
                        detail={target?.provider.name ?? group.deviceId}
                        count={`${group.preloads.length} ${countNoun(group.preloads.length, "app", "apps")}`}
                        platform={group.platform}
                      >
                        {group.preloads.map((preload) => (
                          <PreloadChip
                            key={`${preload.deviceId}:${preload.appId}`}
                            label={preload.appId}
                            detail={`${preload.filename} · ${formatBytes(preload.size)}`}
                            preload={preload}
                            deviceName={deviceName}
                            target={target}
                            removing={removePreload.isPending}
                            isRemoving={
                              removePreload.isPending &&
                              removePreload.variables?.deviceId === preload.deviceId &&
                              removePreload.variables.appId === preload.appId
                            }
                            onRemove={setRemoveTarget}
                          />
                        ))}
                      </PreloadGroup>
                    );
                  })}
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Application package</CardTitle>
          <CardDescription>
            APK files can only target Android devices. IPA files can only target iOS devices. The
            farm retains every preload and restores it during cleanup if it is removed.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <input
            ref={fileInput}
            type="file"
            accept=".apk,.ipa"
            className="sr-only"
            onChange={(event) => chooseFile(event.target.files?.[0])}
          />
          {!file ? (
            <button
              type="button"
              className={cn(
                "flex w-full flex-col items-center justify-center gap-2 rounded-lg border border-dashed px-6 py-12 text-center transition-colors",
                dragging
                  ? "border-primary bg-primary/5"
                  : "border-muted-foreground/30 hover:border-primary hover:bg-accent/40",
              )}
              onClick={() => fileInput.current?.click()}
              onDragEnter={(event) => {
                event.preventDefault();
                setDragging(true);
              }}
              onDragOver={(event) => event.preventDefault()}
              onDragLeave={(event) => {
                event.preventDefault();
                setDragging(false);
              }}
              onDrop={(event) => {
                event.preventDefault();
                setDragging(false);
                chooseFile(event.dataTransfer.files[0]);
              }}
            >
              <Upload className="size-7 text-muted-foreground" />
              <span className="font-medium">Drop an APK or IPA here</span>
              <span className="text-muted-foreground text-sm">or click to browse</span>
            </button>
          ) : (
            <div className="flex items-center gap-3 rounded-lg border bg-muted/20 px-4 py-3">
              <FileArchive className="size-5 shrink-0 text-primary" />
              <div className="min-w-0 flex-1">
                <p className="truncate font-medium">{file.name}</p>
                <p className="text-muted-foreground text-xs">
                  {formatBytes(file.size)} · {platformLabel(platform ?? "")}
                </p>
              </div>
              <Badge variant="outline">{platformLabel(platform ?? "")}</Badge>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label="Remove selected file"
                onClick={clearFile}
              >
                <X className="size-4" />
              </Button>
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div className="flex flex-wrap items-start gap-3">
            <div className="min-w-0 flex-1">
              <CardTitle>Target devices</CardTitle>
              <CardDescription>
                {platform
                  ? `Select available ${platformLabel(platform)} devices. Devices in use are skipped at deployment time.`
                  : "Choose a package first to filter devices by platform."}
              </CardDescription>
            </div>
            <div className="flex gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={!platform || availableDevices.length === 0 || deploying}
                onClick={selectAll}
              >
                {allAvailableSelected ? "Clear available" : "Select available"}
              </Button>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={selectedCount === 0 || deploying}
                onClick={() => setSelected(new Set())}
              >
                Clear selection
              </Button>
            </div>
          </div>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <div className="relative max-w-sm">
            <Search className="-translate-y-1/2 absolute top-1/2 left-2.5 size-4 text-muted-foreground" />
            <Input
              className="pl-8"
              placeholder="Filter devices…"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
          </div>

          {isLoading ? (
            <p className="py-8 text-center text-muted-foreground text-sm">Loading devices…</p>
          ) : filteredDevices.length === 0 ? (
            <p className="rounded-lg border border-dashed py-10 text-center text-muted-foreground text-sm">
              No devices match this filter.
            </p>
          ) : (
            <div className="grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-3">
              {filteredDevices.map((device) => {
                const compatible = Boolean(platform && device.platform === platform);
                const selectable = Boolean(platform && canSelect(device, platform));
                const isSelected = selected.has(device.id);
                return (
                  <button
                    key={device.id}
                    type="button"
                    disabled={(!selectable && !isSelected) || deploying}
                    aria-pressed={isSelected}
                    onClick={() => toggleDevice(device)}
                    className={cn(
                      "flex min-h-24 flex-col gap-2 rounded-lg border p-3 text-left transition-colors focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50",
                      selectable && "hover:border-ring hover:bg-accent/30",
                      isSelected && "border-primary bg-primary/5 ring-1 ring-primary",
                      !selectable && "cursor-not-allowed opacity-55",
                    )}
                  >
                    <div className="flex min-w-0 items-start justify-between gap-2">
                      <span className="min-w-0 truncate font-medium">
                        {device.name ?? device.model ?? device.id}
                      </span>
                      {isSelected && <Check className="size-4 shrink-0 text-primary" />}
                    </div>
                    <div className="flex min-w-0 items-center justify-between gap-2 text-xs">
                      <span className="truncate text-muted-foreground">
                        {device.model ?? device.id}
                      </span>
                      <Badge variant="outline">{platformLabel(device.platform)}</Badge>
                    </div>
                    <p className="truncate text-muted-foreground text-xs">
                      {!platform
                        ? "Choose a package"
                        : !compatible
                          ? `${platformLabel(platform)} only`
                          : device.reservation
                            ? "In use"
                            : device.provider.status !== "online"
                              ? "Provider offline"
                              : device.status === "ready" || device.status === "present"
                                ? "Available"
                                : device.status}
                    </p>
                  </button>
                );
              })}
            </div>
          )}
        </CardContent>
      </Card>

      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-muted-foreground text-sm">
          {selectedCount} device{selectedCount === 1 ? "" : "s"} selected
        </p>
        <Button
          type="button"
          size="lg"
          disabled={!file || !platform || selectedCount === 0 || deploying}
          onClick={() => void deploy()}
        >
          {deploying && <LoaderCircle className="size-4 animate-spin" />}
          {deploying
            ? "Deploying…"
            : `Deploy to ${selectedCount} device${selectedCount === 1 ? "" : "s"}`}
        </Button>
      </div>

      {Object.keys(results).length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle>Deployment results</CardTitle>
            <CardDescription>
              The provider retains the package and repairs the preload during cleanup if someone
              removes the app during a session.
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-3">
            {Object.entries(results).map(([deviceId, result]) => {
              const device = devices?.find((candidate) => candidate.id === deviceId);
              return (
                <div key={deviceId} className="rounded-lg border px-4 py-3">
                  <div className="flex items-center justify-between gap-3">
                    <span className="min-w-0 truncate font-medium">
                      {device?.name ?? device?.model ?? deviceId}
                    </span>
                    <ResultBadge result={result} />
                  </div>
                  {result.state === "uploading" && (
                    <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-muted">
                      <div
                        className="h-full rounded-full bg-primary transition-[width]"
                        style={{ width: `${Math.round(result.progress * 100)}%` }}
                      />
                    </div>
                  )}
                  {result.message && (
                    <p className="mt-1 truncate text-muted-foreground text-xs">{result.message}</p>
                  )}
                </div>
              );
            })}
          </CardContent>
        </Card>
      )}

      <p className="text-muted-foreground text-xs">
        Only idle devices are eligible. The provider checks again before install, then keeps the
        preload under farm control and repairs it during cleanup before the device is released.
      </p>

      <Dialog open={removeTarget !== null} onOpenChange={(open) => !open && setRemoveTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Remove this preloaded app?</DialogTitle>
            <DialogDescription>
              {removeTarget
                ? `${removeTarget.appId} will be uninstalled from ${removeTarget.deviceName} and will no longer be restored during cleanup.`
                : "The app will be uninstalled and removed from preload policy."}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setRemoveTarget(null)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              disabled={!removeTarget || removePreload.isPending}
              onClick={() => removeTarget && removePreload.mutate(removeTarget)}
            >
              {removePreload.isPending && <LoaderCircle className="size-4 animate-spin" />}
              Remove app
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function PreloadGroup({
  title,
  detail,
  count,
  platform,
  children,
}: {
  title: string;
  detail: string;
  count: string;
  platform: PreloadInfo["platform"];
  children: ReactNode;
}) {
  return (
    <section className="overflow-hidden rounded-lg border">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b bg-muted/30 px-3 py-2">
        <div className="min-w-48 flex-1">
          <p className="truncate font-medium text-sm" title={title}>
            {title}
          </p>
          <p className="truncate text-muted-foreground text-xs" title={detail}>
            {detail}
          </p>
        </div>
        <span className="text-muted-foreground text-xs tabular-nums">{count}</span>
        <Badge variant="outline">{platformLabel(platform)}</Badge>
      </div>
      <div className="flex flex-wrap gap-1.5 px-3 py-2">{children}</div>
    </section>
  );
}

function PreloadChip({
  label,
  detail,
  preload,
  deviceName,
  target,
  removing,
  isRemoving,
  onRemove,
}: {
  label: string;
  detail: string;
  preload: PreloadInfo;
  deviceName: string;
  target: DeviceListItem | undefined;
  removing: boolean;
  isRemoving: boolean;
  onRemove: (target: { deviceId: string; appId: string; deviceName: string }) => void;
}) {
  const unavailableReason = preloadUnavailableReason(target);
  const removeTitle =
    unavailableReason ??
    (removing && !isRemoving ? "Another preload is being removed" : "Remove preload");

  return (
    <div
      className="flex h-8 min-w-0 max-w-full items-center rounded-md border bg-background pl-2 shadow-xs"
      title={detail}
    >
      <span className="max-w-64 truncate text-sm">{label}</span>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        className="ml-1 rounded-l-none border-l text-muted-foreground hover:text-destructive"
        disabled={Boolean(unavailableReason) || removing}
        aria-label={`Remove ${preload.appId} from ${deviceName}`}
        title={removeTitle}
        onClick={() =>
          onRemove({
            deviceId: preload.deviceId,
            appId: preload.appId,
            deviceName,
          })
        }
      >
        {isRemoving ? (
          <LoaderCircle className="size-3 animate-spin" />
        ) : (
          <Trash2 className="size-3" />
        )}
      </Button>
    </div>
  );
}

function groupPreloadsByApp(
  preloads: readonly PreloadInfo[],
  deviceById: ReadonlyMap<string, DeviceListItem>,
) {
  const groups = new Map<
    string,
    {
      key: string;
      appId: string;
      platform: PreloadInfo["platform"];
      preloads: PreloadInfo[];
    }
  >();

  for (const preload of preloads) {
    const key = `${preload.platform}:${preload.appId}`;
    const existing = groups.get(key);
    if (existing) existing.preloads.push(preload);
    else {
      groups.set(key, {
        key,
        appId: preload.appId,
        platform: preload.platform,
        preloads: [preload],
      });
    }
  }

  return [...groups.values()]
    .sort((a, b) => a.appId.localeCompare(b.appId))
    .map((group) => ({
      ...group,
      preloads: group.preloads.sort((a, b) =>
        deviceDisplayName(deviceById.get(a.deviceId), a.deviceId).localeCompare(
          deviceDisplayName(deviceById.get(b.deviceId), b.deviceId),
        ),
      ),
    }));
}

function groupPreloadsByDevice(
  preloads: readonly PreloadInfo[],
  deviceById: ReadonlyMap<string, DeviceListItem>,
) {
  const groups = new Map<
    string,
    { deviceId: string; platform: PreloadInfo["platform"]; preloads: PreloadInfo[] }
  >();

  for (const preload of preloads) {
    const existing = groups.get(preload.deviceId);
    if (existing) existing.preloads.push(preload);
    else {
      groups.set(preload.deviceId, {
        deviceId: preload.deviceId,
        platform: preload.platform,
        preloads: [preload],
      });
    }
  }

  return [...groups.values()]
    .sort((a, b) =>
      deviceDisplayName(deviceById.get(a.deviceId), a.deviceId).localeCompare(
        deviceDisplayName(deviceById.get(b.deviceId), b.deviceId),
      ),
    )
    .map((group) => ({
      ...group,
      preloads: group.preloads.sort((a, b) => a.appId.localeCompare(b.appId)),
    }));
}

function packageSummary(preloads: readonly PreloadInfo[]): string {
  const variants = new Map(preloads.map((preload) => [preload.sha256, preload]));
  if (variants.size !== 1) return `${variants.size} package variants`;
  const preload = variants.values().next().value;
  return preload ? `${preload.filename} · ${formatBytes(preload.size)}` : "";
}

function deviceDisplayName(device: DeviceListItem | undefined, fallback: string): string {
  return device?.name ?? device?.model ?? fallback;
}

function preloadUnavailableReason(device: DeviceListItem | undefined): string | null {
  if (!device) return "Device is unavailable";
  if (device.reservation) return "Device is in use";
  if (device.provider.status !== "online") return "Provider is offline";
  if (!DEPLOYABLE_STATUSES.has(device.status)) return `Device is ${device.status}`;
  return null;
}

function countNoun(count: number, singular: string, plural: string): string {
  return count === 1 ? singular : plural;
}

function appPlatform(filename: string): AppPlatform | null {
  const lower = filename.toLowerCase();
  if (lower.endsWith(".apk")) return "android";
  if (lower.endsWith(".ipa")) return "ios";
  return null;
}

function canSelect(device: DeviceListItem, platform: AppPlatform): boolean {
  return (
    device.platform === platform &&
    device.provider.status === "online" &&
    !device.reservation &&
    DEPLOYABLE_STATUSES.has(device.status)
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "Deployment failed";
}

function ResultBadge({ result }: { result: DeploymentResult }) {
  const labels: Record<DeploymentState, string> = {
    queued: "queued",
    uploading: "uploading",
    success: "installed",
    failed: "failed",
    skipped: "skipped",
  };
  return (
    <Badge
      variant="outline"
      className={cn(
        result.state === "success" && "border-success/30 bg-success/15 text-success",
        result.state === "failed" && "border-destructive/30 bg-destructive/15 text-destructive",
        result.state === "skipped" && "text-muted-foreground",
        result.state === "uploading" && "border-info/30 bg-info/15 text-info",
      )}
    >
      {labels[result.state]}
    </Badge>
  );
}
