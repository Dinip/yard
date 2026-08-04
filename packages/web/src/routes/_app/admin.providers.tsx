import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, redirect } from "@tanstack/react-router";
import { Check, Copy, KeyRound, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { trpc } from "@/lib/trpc";
import { relativeTime } from "@/lib/utils";

export const Route = createFileRoute("/_app/admin/providers")({
  beforeLoad: ({ context }) => {
    if (context.user.role !== "admin") throw redirect({ to: "/devices" });
  },
  component: AdminProvidersPage,
});

function AdminProvidersPage() {
  const qc = useQueryClient();
  const { data: providers } = useQuery({
    ...trpc.provider.list.queryOptions(),
    refetchInterval: 10_000,
  });

  const [creating, setCreating] = useState(false);
  const [tokensFor, setTokensFor] = useState<string | null>(null);
  const refresh = () => qc.invalidateQueries({ queryKey: trpc.provider.list.queryKey() });

  const remove = useMutation(
    trpc.provider.remove.mutationOptions({
      onSuccess: () => {
        toast.success("Provider deleted");
        refresh();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center gap-3">
        <h1 className="font-semibold text-2xl">Providers</h1>
        <div className="flex-1" />
        <Button onClick={() => setCreating(true)}>
          <Plus className="size-4" />
          Add provider
        </Button>
      </div>

      {!providers?.length ? (
        <div className="rounded-lg border border-dashed py-16 text-center">
          <p className="font-medium">No providers yet</p>
          <p className="mx-auto mt-1 max-w-md text-muted-foreground text-sm">
            Register a provider here, issue it a token, then start the provider with that token. It
            dials out to the coordinator — no inbound access needed.
          </p>
        </div>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>ID</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Devices</TableHead>
              <TableHead>Public base URL</TableHead>
              <TableHead>Version</TableHead>
              <TableHead>Last seen</TableHead>
              <TableHead className="w-24" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {providers.map((p) => (
              <TableRow key={p.id}>
                <TableCell className="font-medium">{p.name}</TableCell>
                <TableCell className="font-mono text-xs">{p.id}</TableCell>
                <TableCell>
                  <Badge
                    variant="outline"
                    className={
                      p.connected
                        ? "border-success/30 bg-success/15 text-success"
                        : "text-muted-foreground"
                    }
                  >
                    {p.connected ? "connected" : "offline"}
                  </Badge>
                </TableCell>
                <TableCell>{p.deviceCount}</TableCell>
                <TableCell className="font-mono text-xs">{p.publicBaseUrl}</TableCell>
                <TableCell className="text-muted-foreground">{p.version ?? "—"}</TableCell>
                <TableCell className="text-muted-foreground">
                  {relativeTime(p.lastSeenAt)}
                </TableCell>
                <TableCell>
                  <div className="flex gap-1">
                    <Button variant="ghost" size="icon" onClick={() => setTokensFor(p.id)}>
                      <KeyRound className="size-4" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => {
                        if (confirm(`Delete provider "${p.name}" and all its devices?`)) {
                          remove.mutate({ id: p.id });
                        }
                      }}
                    >
                      <Trash2 className="size-4 text-destructive" />
                    </Button>
                  </div>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}

      <CreateProviderDialog open={creating} onOpenChange={setCreating} onCreated={refresh} />
      <TokensDialog providerId={tokensFor} onOpenChange={() => setTokensFor(null)} />
    </div>
  );
}

function CreateProviderDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  onCreated: () => void;
}) {
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const [publicBaseUrl, setPublicBaseUrl] = useState("https://");

  const create = useMutation(
    trpc.provider.create.mutationOptions({
      onSuccess: () => {
        toast.success("Provider created — issue it a token next");
        onCreated();
        onOpenChange(false);
        setId("");
        setName("");
        setPublicBaseUrl("https://");
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add provider</DialogTitle>
          <DialogDescription>
            The provider dials out to the coordinator, so it needs no inbound access. The public
            base URL is what the <em>browser</em> uses to reach it directly for video and uploads —
            the coordinator never connects to it.
          </DialogDescription>
        </DialogHeader>

        <form
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate({ id, name, publicBaseUrl });
          }}
        >
          <div className="grid gap-1.5">
            <Label htmlFor="p-id">Identifier</Label>
            <Input
              id="p-id"
              required
              placeholder="lab-1"
              pattern="[a-z0-9][a-z0-9-]*"
              value={id}
              onChange={(e) => setId(e.target.value)}
            />
            <p className="text-muted-foreground text-xs">
              Lowercase letters, digits and dashes. Cannot be changed later.
            </p>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="p-name">Name</Label>
            <Input
              id="p-name"
              required
              placeholder="Lab 1"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="p-url">Public base URL</Label>
            <Input
              id="p-url"
              required
              type="url"
              placeholder="https://lab-1.example.com"
              value={publicBaseUrl}
              onChange={(e) => setPublicBaseUrl(e.target.value)}
            />
            <p className="text-muted-foreground text-xs">
              Must be HTTPS in production — WebCodecs requires a secure context.
            </p>
          </div>

          <DialogFooter>
            <Button type="submit" disabled={create.isPending}>
              Create
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function TokensDialog({
  providerId,
  onOpenChange,
}: {
  providerId: string | null;
  onOpenChange: (v: boolean) => void;
}) {
  const qc = useQueryClient();
  const [label, setLabel] = useState("");
  /** Held in component state only: the server cannot show it again. */
  const [issued, setIssued] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const { data: tokens } = useQuery({
    ...trpc.provider.tokens.list.queryOptions({ providerId: providerId ?? "" }),
    enabled: !!providerId,
  });

  const refresh = () => qc.invalidateQueries({ queryKey: trpc.provider.tokens.list.queryKey() });

  const create = useMutation(
    trpc.provider.tokens.create.mutationOptions({
      onSuccess: (result) => {
        setIssued(result.token);
        setLabel("");
        refresh();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  const revoke = useMutation(
    trpc.provider.tokens.revoke.mutationOptions({
      onSuccess: () => {
        toast.success("Token revoked");
        refresh();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  return (
    <Dialog
      open={!!providerId}
      onOpenChange={(v) => {
        if (!v) {
          setIssued(null);
          setCopied(false);
        }
        onOpenChange(v);
      }}
    >
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Provider tokens</DialogTitle>
          <DialogDescription>
            Machine credentials for <span className="font-mono text-xs">{providerId}</span>. Only a
            hash is stored, so a token is shown exactly once.
          </DialogDescription>
        </DialogHeader>

        {issued && (
          <div className="flex flex-col gap-2 rounded-md border border-warning/40 bg-warning/10 p-3">
            <p className="font-medium text-sm">Copy this now — it will not be shown again.</p>
            <div className="flex items-center gap-2">
              <code className="min-w-0 flex-1 truncate rounded bg-background px-2 py-1.5 font-mono text-xs">
                {issued}
              </code>
              <Button
                size="icon"
                variant="outline"
                onClick={async () => {
                  await navigator.clipboard.writeText(issued);
                  setCopied(true);
                  toast.success("Copied");
                }}
              >
                {copied ? <Check className="size-4" /> : <Copy className="size-4" />}
              </Button>
            </div>
          </div>
        )}

        <form
          className="flex items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (providerId) create.mutate({ providerId, label: label || undefined });
          }}
        >
          <div className="grid flex-1 gap-1.5">
            <Label htmlFor="t-label">Label (optional)</Label>
            <Input
              id="t-label"
              placeholder="lab-1 host"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
            />
          </div>
          <Button type="submit" disabled={create.isPending}>
            Issue token
          </Button>
        </form>

        <div className="flex flex-col gap-1">
          {tokens?.length ? (
            tokens.map((t) => (
              <div
                key={t.id}
                className="flex items-center gap-2 rounded-md border px-3 py-2 text-sm"
              >
                <div className="min-w-0 flex-1">
                  <p className="truncate">{t.label ?? "unlabelled"}</p>
                  <p className="text-muted-foreground text-xs">
                    created {relativeTime(t.createdAt)} · last used {relativeTime(t.lastUsedAt)}
                  </p>
                </div>
                {t.revokedAt ? (
                  <Badge variant="secondary">revoked</Badge>
                ) : (
                  <Button
                    size="sm"
                    variant="ghost"
                    className="text-destructive"
                    onClick={() => revoke.mutate({ tokenId: t.id })}
                  >
                    Revoke
                  </Button>
                )}
              </div>
            ))
          ) : (
            <p className="py-2 text-muted-foreground text-sm">No tokens yet.</p>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
