import { AUDIT_ACTION_VALUES, auditActionLabel, auditActionsByGroup } from "@farm/protocol";
import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link, redirect } from "@tanstack/react-router";
import { X } from "lucide-react";
import { useEffect, useState } from "react";
import { z } from "zod";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
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

/** Sentinel for "no filter": `<Select>` cannot carry an empty-string value. */
const ANY = "any";

/**
 * Filters live in the URL, not in component state, so a filtered view is
 * something you can send to someone. That is most of the point of an audit log
 * with filters at all — "look at what happened to this device" is a link.
 */
const search = z.object({
  action: z.enum(AUDIT_ACTION_VALUES).optional(),
  actorUserId: z.string().optional(),
  targetId: z.string().optional(),
  from: z.string().optional(),
  to: z.string().optional(),
  page: z.number().int().min(0).catch(0).default(0),
});

type Search = z.infer<typeof search>;

export const Route = createFileRoute("/_app/admin/audit")({
  beforeLoad: ({ context }) => {
    // A UX guard, not a security boundary — `adminProcedure` is the real one.
    if (context.user?.role !== "admin") throw redirect({ to: "/devices" });
  },
  validateSearch: search,
  component: AuditPage,
});

function AuditPage() {
  const params = Route.useSearch();
  const navigate = Route.useNavigate();
  /** The row whose whole record is on screen; the table itself only clamps. */
  const [openEntry, setOpenEntry] = useState<AuditEntry | null>(null);

  /** Any filter change resets to the first page: page 4 of a new query is nothing. */
  const setFilter = (patch: Partial<Search>) =>
    navigate({ search: (current) => ({ ...current, ...patch, page: 0 }) });

  const { data, isFetching } = useQuery(
    trpc.admin.audit.queryOptions({
      limit: PAGE_SIZE,
      offset: params.page * PAGE_SIZE,
      action: params.action ? [params.action] : undefined,
      actorUserId: params.actorUserId,
      targetId: params.targetId,
      from: params.from ? new Date(params.from) : undefined,
      // A date input means the whole day, not midnight at its start.
      to: params.to ? endOfDay(params.to) : undefined,
    }),
  );

  const { data: users } = useQuery(trpc.admin.users.queryOptions({ limit: 200, offset: 0 }));

  const entries = data?.items ?? [];
  const total = data?.total ?? 0;
  const first = total === 0 ? 0 : params.page * PAGE_SIZE + 1;
  const last = params.page * PAGE_SIZE + entries.length;
  const filtered = Boolean(
    params.action || params.actorUserId || params.targetId || params.from || params.to,
  );

  return (
    <div className="flex flex-col gap-6">
      <div>
        <h1 className="font-semibold text-2xl">Audit log</h1>
        <p className="text-muted-foreground text-sm">
          Every reservation, release and install. Installs carry the digest of a file that no longer
          exists anywhere.
        </p>
      </div>

      <div className="flex flex-wrap items-end gap-3">
        <Field label="Who" htmlFor="filter-who">
          <Select
            value={params.actorUserId ?? ANY}
            onValueChange={(value) => setFilter({ actorUserId: value === ANY ? undefined : value })}
          >
            <SelectTrigger id="filter-who" className="w-52">
              <SelectValue placeholder="Anyone" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ANY}>Anyone</SelectItem>
              {users?.users.map((u) => (
                <SelectItem key={u.id} value={u.id}>
                  {u.name ?? u.email}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>

        <Field label="Action" htmlFor="filter-action">
          <Select
            value={params.action ?? ANY}
            onValueChange={(value) =>
              setFilter({
                action: value === ANY ? undefined : (value as NonNullable<Search["action"]>),
              })
            }
          >
            <SelectTrigger id="filter-action" className="w-60">
              <SelectValue placeholder="All actions" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ANY}>All actions</SelectItem>
              {auditActionsByGroup().map(({ group, actions }) => (
                <SelectGroup key={group}>
                  <SelectLabel>{group}</SelectLabel>
                  {actions.map((action) => (
                    <SelectItem key={action.value} value={action.value}>
                      {action.label}
                    </SelectItem>
                  ))}
                </SelectGroup>
              ))}
            </SelectContent>
          </Select>
        </Field>

        <TargetFilter value={params.targetId} onChange={(targetId) => setFilter({ targetId })} />

        <Field label="From" htmlFor="filter-from">
          <Input
            id="filter-from"
            type="date"
            className="w-40"
            value={params.from ?? ""}
            onChange={(e) => setFilter({ from: e.target.value || undefined })}
          />
        </Field>

        <Field label="To" htmlFor="filter-to">
          <Input
            id="filter-to"
            type="date"
            className="w-40"
            value={params.to ?? ""}
            onChange={(e) => setFilter({ to: e.target.value || undefined })}
          />
        </Field>

        {filtered && (
          <Button
            variant="ghost"
            size="sm"
            onClick={() =>
              navigate({
                search: {
                  page: 0,
                  action: undefined,
                  actorUserId: undefined,
                  targetId: undefined,
                  from: undefined,
                  to: undefined,
                },
              })
            }
          >
            <X className="size-4" />
            Clear
          </Button>
        )}
      </div>

      {/* `table-fixed` with a width on every column is what actually stops one
          long metadata blob from widening the whole table: under the default
          auto layout the widest cell sets the column, and a `max-w-*` on the
          cell never binds. Detail takes whatever is left and truncates. */}
      <Table className="table-fixed">
        <TableHeader>
          <TableRow>
            <TableHead className="w-28">When</TableHead>
            <TableHead className="w-36">Who</TableHead>
            {/* Wide enough for the longest label, "Released at the session
                limit" — a badge clipped mid-word looks like a broken border
                rather than truncated text. */}
            <TableHead className="w-56">Action</TableHead>
            <TableHead className="w-56">Target</TableHead>
            <TableHead>Detail</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {entries.map((entry) => (
            // The whole row opens the entry. `tabIndex` and the key handler are
            // what make that reachable without a mouse, now that there is no
            // button in a trailing column to tab to.
            <TableRow
              key={entry.id}
              tabIndex={0}
              aria-label={`Show the whole entry: ${auditActionLabel(entry.action)}`}
              className="cursor-pointer focus-visible:bg-muted/50 focus-visible:outline-none"
              onClick={() => setOpenEntry(entry)}
              onKeyDown={(event) => {
                if (event.key !== "Enter" && event.key !== " ") return;
                // Space scrolls the page otherwise, and the target link inside
                // the row has its own Enter behaviour.
                if (event.target !== event.currentTarget) return;
                event.preventDefault();
                setOpenEntry(entry);
              }}
            >
              <TableCell className="text-muted-foreground">
                <span title={new Date(entry.at).toLocaleString()}>{relativeTime(entry.at)}</span>
              </TableCell>
              <TableCell className="truncate">
                <Actor entry={entry} />
              </TableCell>
              <TableCell>
                <Badge variant="outline" className="max-w-full truncate" title={entry.action}>
                  {auditActionLabel(entry.action)}
                </Badge>
              </TableCell>
              <TableCell className="truncate font-mono text-xs">
                <Target entry={entry} />
              </TableCell>
              <TableCell className="truncate">
                <Detail entry={entry} />
              </TableCell>
            </TableRow>
          ))}
          {entries.length === 0 && (
            <TableRow>
              <TableCell colSpan={5} className="py-10 text-center text-muted-foreground">
                {isFetching
                  ? "Loading…"
                  : filtered
                    ? "Nothing matches these filters."
                    : "Nothing recorded yet."}
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>

      <AuditEntryDialog entry={openEntry} onClose={() => setOpenEntry(null)} />

      <div className="flex items-center justify-between">
        <span className="text-muted-foreground text-sm">
          {total === 0 ? "No entries" : `${first}–${last} of ${total}`}
        </span>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={params.page === 0}
            onClick={() => navigate({ search: (c) => ({ ...c, page: c.page - 1 }) })}
          >
            Previous
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={last >= total}
            onClick={() => navigate({ search: (c) => ({ ...c, page: c.page + 1 }) })}
          >
            Next
          </Button>
        </div>
      </div>
    </div>
  );
}

/**
 * Debounced, because this one is typed rather than picked: a request per
 * keystroke would also mean a URL history entry per keystroke.
 */
function TargetFilter({
  value,
  onChange,
}: {
  value: string | undefined;
  onChange: (value: string | undefined) => void;
}) {
  const [draft, setDraft] = useState(value ?? "");

  // Follow the URL when it changes from outside — Clear, or a pasted link.
  useEffect(() => setDraft(value ?? ""), [value]);

  useEffect(() => {
    const current = value ?? "";
    if (draft === current) return;
    const timer = setTimeout(() => onChange(draft.trim() || undefined), 300);
    return () => clearTimeout(timer);
  }, [draft, value, onChange]);

  return (
    <Field label="Target" htmlFor="filter-target">
      <Input
        id="filter-target"
        className="w-56"
        placeholder="Device or provider id"
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
      />
    </Field>
  );
}

function Field({
  label,
  htmlFor,
  children,
}: {
  label: string;
  htmlFor: string;
  children: React.ReactNode;
}) {
  return (
    <div className="grid gap-1.5">
      <Label htmlFor={htmlFor} className="text-muted-foreground text-xs">
        {label}
      </Label>
      {children}
    </div>
  );
}

/** The last moment of the given day, so "to: today" includes today. */
function endOfDay(date: string): Date {
  const end = new Date(date);
  end.setHours(23, 59, 59, 999);
  return end;
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

/** A device target is a link: the row is usually the start of a question. */
function Target({ entry }: { entry: AuditEntry }) {
  if (!entry.targetId) return <span className="text-muted-foreground">—</span>;
  if (entry.targetType !== "device") return <span>{entry.targetId}</span>;
  return (
    <Link
      to="/devices/$deviceId"
      params={{ deviceId: entry.targetId }}
      className="hover:underline"
      // The row opens the entry; this link goes somewhere else entirely.
      onClick={(event) => event.stopPropagation()}
    >
      {entry.targetId}
    </Link>
  );
}

/**
 * One line, always.
 *
 * The cell is `truncate` and the column has a fixed width, so anything longer
 * is cut rather than allowed to set the table's width. The whole value is a
 * click away in `AuditEntryDialog` — nothing here is the only copy.
 */
function Detail({ entry }: { entry: AuditEntry }) {
  const metadata = entry.metadata as Record<string, unknown> | null;
  if (!metadata) return <span className="text-muted-foreground">—</span>;

  if (entry.action === "device.install") {
    return (
      <span className="text-xs">
        {String(metadata.filename ?? "unknown")}
        {typeof metadata.size === "number" && ` · ${formatBytes(metadata.size)}`}
        {metadata.ok === false && <span className="ml-1 text-destructive">failed</span>}
      </span>
    );
  }

  const reason = typeof metadata.reason === "string" ? metadata.reason : null;
  if (reason) return <span className="text-xs">{reason}</span>;

  return (
    <span className="font-mono text-muted-foreground text-xs">{JSON.stringify(metadata)}</span>
  );
}

/**
 * The whole record, for when the row's one line was not enough.
 *
 * Every value here is shown in full and is selectable: an install's sha256 is
 * the only surviving trace of a file that was deleted after it was installed,
 * so it must be copyable, not merely visible.
 */
function AuditEntryDialog({ entry, onClose }: { entry: AuditEntry | null; onClose: () => void }) {
  const metadata = entry?.metadata as Record<string, unknown> | null | undefined;

  return (
    <Dialog open={entry !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{entry ? auditActionLabel(entry.action) : ""}</DialogTitle>
          <DialogDescription className="font-mono text-xs">{entry?.action}</DialogDescription>
        </DialogHeader>

        {entry && (
          <div className="grid gap-3 text-sm">
            <Row label="When">
              {new Date(entry.at).toLocaleString()}{" "}
              <span className="text-muted-foreground">({relativeTime(entry.at)})</span>
            </Row>
            <Row label="Who">
              <Actor entry={entry} />
            </Row>
            <Row label="Target">
              <span className="font-mono text-xs">
                {entry.targetType ? `${entry.targetType} · ` : ""}
                <Target entry={entry} />
              </span>
            </Row>
            <Row label="Detail">
              {metadata ? (
                <pre className="max-h-80 overflow-auto rounded bg-muted p-3 font-mono text-xs">
                  {JSON.stringify(metadata, null, 2)}
                </pre>
              ) : (
                <span className="text-muted-foreground">—</span>
              )}
            </Row>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid gap-1">
      <span className="text-muted-foreground text-xs">{label}</span>
      <div className="min-w-0">{children}</div>
    </div>
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
