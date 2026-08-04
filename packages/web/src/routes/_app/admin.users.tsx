import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, redirect } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { authClient } from "@/lib/auth-client";
import { trpc } from "@/lib/trpc";
import { relativeTime } from "@/lib/utils";

export const Route = createFileRoute("/_app/admin/users")({
  beforeLoad: ({ context }) => {
    if (context.user.role !== "admin") throw redirect({ to: "/devices" });
  },
  component: AdminUsersPage,
});

function AdminUsersPage() {
  const qc = useQueryClient();
  const [search, setSearch] = useState("");
  const { data } = useQuery(trpc.admin.users.queryOptions({ search, limit: 50, offset: 0 }));

  const refresh = () => qc.invalidateQueries({ queryKey: trpc.admin.users.queryKey() });

  // Role and ban changes go through better-auth's admin plugin rather than a
  // tRPC procedure — it owns the session invalidation that must follow them.
  const setRole = useMutation({
    mutationFn: ({ userId, role }: { userId: string; role: "admin" | "user" }) =>
      authClient.admin.setRole({ userId, role }),
    onSuccess: () => {
      toast.success("Role updated");
      refresh();
    },
    onError: (e: Error) => toast.error(e.message),
  });

  const toggleBan = useMutation({
    mutationFn: ({ userId, banned }: { userId: string; banned: boolean }) =>
      banned
        ? authClient.admin.unbanUser({ userId })
        : authClient.admin.banUser({ userId, banReason: "Banned by admin" }),
    onSuccess: () => {
      toast.success("Updated");
      refresh();
    },
    onError: (e: Error) => toast.error(e.message),
  });

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center gap-3">
        <h1 className="font-semibold text-2xl">Users</h1>
        <span className="text-muted-foreground text-sm">{data?.total ?? 0} total</span>
        <div className="flex-1" />
        <Input
          className="w-64"
          placeholder="Search name or email…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
      </div>

      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Email</TableHead>
            <TableHead>Role</TableHead>
            <TableHead>Status</TableHead>
            <TableHead>Joined</TableHead>
            <TableHead className="w-10" />
          </TableRow>
        </TableHeader>
        <TableBody>
          {data?.users.map((u) => (
            <TableRow key={u.id}>
              <TableCell className="font-medium">{u.name}</TableCell>
              <TableCell className="text-muted-foreground">{u.email}</TableCell>
              <TableCell>
                <Badge variant={u.role === "admin" ? "default" : "secondary"}>
                  {u.role ?? "user"}
                </Badge>
              </TableCell>
              <TableCell>
                {u.banned ? (
                  <Badge variant="destructive">banned</Badge>
                ) : (
                  <span className="text-muted-foreground text-sm">active</span>
                )}
              </TableCell>
              <TableCell className="text-muted-foreground">{relativeTime(u.createdAt)}</TableCell>
              <TableCell>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button variant="ghost" size="sm">
                      …
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem
                      onSelect={() =>
                        setRole.mutate({
                          userId: u.id,
                          role: u.role === "admin" ? "user" : "admin",
                        })
                      }
                    >
                      {u.role === "admin" ? "Demote to user" : "Promote to admin"}
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      variant="destructive"
                      onSelect={() => toggleBan.mutate({ userId: u.id, banned: !!u.banned })}
                    >
                      {u.banned ? "Unban" : "Ban"}
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}
