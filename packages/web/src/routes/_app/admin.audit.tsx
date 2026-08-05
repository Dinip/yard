import { useQuery } from "@tanstack/react-query";
import { createFileRoute, redirect } from "@tanstack/react-router";
import { useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { trpc } from "@/lib/trpc";
import type { AuditEntry } from "@/lib/types";
import { relativeTime } from "@/lib/utils";

const PAGE_SIZE = 100;

/**
 * There is no artifact storage anywhere in this system: an APK is streamed to
 * the provider, installed, and deleted. These rows are the only record that an
 * install ever happened, which is why the digest is shown rather than hidden
 * behind a detail view.
 */
const ACTIONS = [
  { value: "", label: "All actions" },
  { value: "device.install", label: "Installs" },
  { value: "device.reserve", label: "Reservations" },
  { value: "device.release", label: "Releases" },
  { value: "device.force_release", label: "Force releases" },
  { value: "device.reservation_expired", label: "Expired reservations" },
  { value: "provider.token.create", label: "Tokens issued" },
  { value: "provider.token.revoke", label: "Tokens revoked" },
];

export const Route = createFileRoute("/_app/admin/audit")({
  beforeLoad: ({ context }) => {
    // A UX guard, not a security boundary — `adminProcedure` is the real one.
    if (context.user?.role !== "admin") throw redirect({ to: "/devices" });
  },
  component: AuditPage,
});

function AuditPage() {
  const [action, setAction] = useState("");
  const [page, setPage] = useState(0);

  const { data: entries = [], isFetching } = useQuery(
    trpc.admin.audit.queryOptions({
      limit: PAGE_SIZE,
      offset: page * PAGE_SIZE,
      action: action || undefined,
    }),
  );

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center gap-3">
        <div>
          <h1 className="font-semibold text-2xl">Audit log</h1>
          <p className="text-muted-foreground text-sm">
            Every reservation, release and install. Installs carry the digest of a file that no
            longer exists anywhere.
          </p>
        </div>
        <div className="flex-1" />
        <Select
          value={action}
          onValueChange={(value) => {
            setAction(value);
            setPage(0);
          }}
        >
          <SelectTrigger className="w-56">
            <SelectValue placeholder="All actions" />
          </SelectTrigger>
          <SelectContent>
            {ACTIONS.map((option) => (
              <SelectItem key={option.value || "all"} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>When</TableHead>
            <TableHead>Who</TableHead>
            <TableHead>Action</TableHead>
            <TableHead>Target</TableHead>
            <TableHead>Detail</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {entries.map((entry) => (
            <TableRow key={entry.id}>
              <TableCell className="whitespace-nowrap text-muted-foreground">
                <span title={new Date(entry.at).toLocaleString()}>{relativeTime(entry.at)}</span>
              </TableCell>
              <TableCell>
                <Actor entry={entry} />
              </TableCell>
              <TableCell>
                <Badge variant="outline" className="font-mono text-xs">
                  {entry.action}
                </Badge>
              </TableCell>
              <TableCell className="font-mono text-xs">{entry.targetId ?? "—"}</TableCell>
              <TableCell className="max-w-md">
                <Detail entry={entry} />
              </TableCell>
            </TableRow>
          ))}
          {entries.length === 0 && (
            <TableRow>
              <TableCell colSpan={5} className="py-10 text-center text-muted-foreground">
                {isFetching ? "Loading…" : "Nothing recorded yet."}
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>

      <div className="flex items-center justify-between">
        <span className="text-muted-foreground text-sm">
          {page > 0 && `Page ${page + 1} · `}
          {entries.length} entries
        </span>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={page === 0}
            onClick={() => setPage((current) => current - 1)}
          >
            Previous
          </Button>
          <Button
            variant="outline"
            size="sm"
            // A full page probably means there is another; a short one cannot.
            disabled={entries.length < PAGE_SIZE}
            onClick={() => setPage((current) => current + 1)}
          >
            Next
          </Button>
        </div>
      </div>
    </div>
  );
}

/**
 * Who did it.
 *
 * Three cases, and an audit log must not conflate them: the reaper and provider
 * disconnects have no actor at all, a deleted user leaves an id with no row,
 * and everyone else has a name. Showing "system" for a deleted user would say
 * nobody did something a person did.
 */
function Actor({ entry }: { entry: AuditEntry }) {
  if (entry.actorName) return <span>{entry.actorName}</span>;
  if (entry.actorUserId) {
    return (
      <span className="font-mono text-muted-foreground text-xs" title={entry.actorUserId}>
        deleted user
      </span>
    );
  }
  return <span className="text-muted-foreground">system</span>;
}

function Detail({ entry }: { entry: AuditEntry }) {
  const metadata = entry.metadata as Record<string, unknown> | null;
  if (!metadata) return <span className="text-muted-foreground">—</span>;

  if (entry.action === "device.install") {
    const sha = typeof metadata.sha256 === "string" ? metadata.sha256 : null;
    return (
      <div className="flex flex-col gap-0.5 text-xs">
        <span className="truncate">
          {String(metadata.filename ?? "unknown")}
          {typeof metadata.size === "number" && ` · ${formatBytes(metadata.size)}`}
          {metadata.ok === false && <span className="ml-1 text-destructive">failed</span>}
        </span>
        {sha && (
          <span className="truncate font-mono text-muted-foreground" title={sha}>
            {sha}
          </span>
        )}
      </div>
    );
  }

  const reason = typeof metadata.reason === "string" ? metadata.reason : null;
  if (reason) return <span className="text-xs">{reason}</span>;

  return (
    <span className="truncate font-mono text-muted-foreground text-xs">
      {JSON.stringify(metadata)}
    </span>
  );
}

function formatBytes(size: number) {
  if (size < 1024) return `${size} B`;
  const units = ["KB", "MB", "GB"];
  let value = size / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}
