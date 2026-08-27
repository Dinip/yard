import { Link } from "@tanstack/react-router";
import { Battery, Smartphone } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardFooter, CardHeader } from "@/components/ui/card";
import type { DeviceListItem } from "@/lib/types";
import { cn, platformLabel } from "@/lib/utils";

const STATUS_STYLES: Record<string, string> = {
  ready: "bg-success/15 text-success border-success/30",
  busy: "bg-warning/15 text-warning border-warning/30",
  // Transient and about to be ready, not broken — so it reads as work in
  // progress rather than borrowing the destructive palette.
  cleaning: "bg-info/15 text-info border-info/30",
  preparing: "bg-muted text-muted-foreground",
  present: "bg-muted text-muted-foreground",
  unhealthy: "bg-destructive/15 text-destructive border-destructive/30",
  absent: "bg-muted text-muted-foreground opacity-70",
};

export function DeviceCard({ device, mine }: { device: DeviceListItem; mine?: boolean }) {
  const unavailable = device.status === "absent" || device.status === "unhealthy";

  return (
    <Link
      to="/devices/$deviceId"
      params={{ deviceId: device.id }}
      className="block h-full rounded-xl focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
    >
      <Card
        className={cn(
          "h-full gap-3 py-4 transition-colors hover:border-ring",
          unavailable && "opacity-60",
          // A held device is the one card the user came here to find, so it
          // carries the accent on the whole card rather than a badge alone.
          mine && "border-mine/50 bg-mine/5 hover:border-mine",
        )}
      >
        <CardHeader className="px-4">
          <div className="flex min-w-0 items-start justify-between gap-2">
            <div className="min-w-0">
              <span
                title={device.name ?? device.model ?? device.id}
                className="block truncate font-medium"
              >
                {device.name ?? device.model ?? device.id}
              </span>
              <p className="truncate text-muted-foreground text-xs">
                {device.model ?? "unknown model"}
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-1.5">
              {mine && (
                <Badge className="bg-mine/15 text-mine" variant="outline">
                  Yours
                </Badge>
              )}
              <Badge variant="outline" className={cn(STATUS_STYLES[device.status])}>
                {device.status}
              </Badge>
            </div>
          </div>
        </CardHeader>

        <CardContent className="grid gap-1 px-4 text-muted-foreground text-xs">
          <Row label="Platform">
            <span className="inline-flex items-center gap-1">
              <Smartphone className="size-3" />
              {platformLabel(device.platform)} {device.osVersion}
            </span>
          </Row>
          {device.displayWidth && device.displayHeight ? (
            <Row label="Display">
              {device.displayWidth}×{device.displayHeight}
              {device.displayScale ? ` @${device.displayScale}x` : ""}
            </Row>
          ) : null}
          {device.batteryLevel != null ? (
            <Row label="Battery">
              <span className="inline-flex items-center gap-1">
                <Battery className="size-3" />
                {Math.round(device.batteryLevel * 100)}%
              </span>
            </Row>
          ) : null}
          <Row label="Provider">{device.provider.name}</Row>
        </CardContent>

        <CardFooter className="px-4">
          {mine ? (
            <p className="truncate font-medium text-mine text-xs">Reserved by you</p>
          ) : device.reservation ? (
            <p className="truncate text-muted-foreground text-xs">
              In use by <span className="text-foreground">{device.reservation.ownerName}</span>
            </p>
          ) : (
            <p className="text-muted-foreground text-xs">Available</p>
          )}
        </CardFooter>
      </Card>
    </Link>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex justify-between gap-2">
      <span>{label}</span>
      <span className="truncate text-foreground">{children}</span>
    </div>
  );
}
