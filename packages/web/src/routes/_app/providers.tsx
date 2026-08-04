import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Badge } from "@/components/ui/badge";
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

export const Route = createFileRoute("/_app/providers")({
  loader: ({ context }) =>
    context.queryClient.ensureQueryData(context.trpc.provider.list.queryOptions()),
  component: ProvidersPage,
});

function ProvidersPage() {
  const { data: providers } = useQuery({
    ...trpc.provider.list.queryOptions(),
    refetchInterval: 10_000,
  });

  return (
    <div className="flex flex-col gap-6">
      <h1 className="font-semibold text-2xl">Providers</h1>

      {!providers?.length ? (
        <div className="rounded-lg border border-dashed py-20 text-center">
          <p className="font-medium">No providers connected</p>
          <p className="mt-1 text-muted-foreground text-sm">
            Provider token issuance and the control-plane gateway land in phase 2.
          </p>
        </div>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Devices</TableHead>
              <TableHead>Public base URL</TableHead>
              <TableHead>Version</TableHead>
              <TableHead>Last seen</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {providers.map((p) => (
              <TableRow key={p.id}>
                <TableCell className="font-medium">{p.name}</TableCell>
                <TableCell>
                  <Badge variant={p.status === "online" ? "default" : "secondary"}>
                    {p.status}
                  </Badge>
                </TableCell>
                <TableCell>{p.deviceCount}</TableCell>
                <TableCell className="font-mono text-xs">{p.publicBaseUrl}</TableCell>
                <TableCell className="text-muted-foreground">{p.version ?? "—"}</TableCell>
                <TableCell className="text-muted-foreground">
                  {relativeTime(p.lastSeenAt)}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </div>
  );
}
