import { useQuery } from "@tanstack/react-query";
import { Link, useRouter } from "@tanstack/react-router";
import { LogOut, Server, Shield, Smartphone } from "lucide-react";
import type { ReactNode } from "react";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { authClient } from "@/lib/auth-client";
import { trpc } from "@/lib/trpc";

export function AppShell({ children }: { children: ReactNode }) {
  const router = useRouter();
  const { data: me } = useQuery(trpc.user.me.queryOptions());

  return (
    <div className="flex min-h-svh flex-col">
      <header className="sticky top-0 z-40 border-b bg-background/80 backdrop-blur">
        <div className="mx-auto flex h-14 w-full max-w-[1600px] items-center gap-1 px-4">
          <Link to="/devices" className="mr-4 flex items-center gap-2 font-semibold">
            <Smartphone className="size-5" />
            <span>{"Device Farm"}</span>
          </Link>

          <NavLink to="/devices" icon={<Smartphone className="size-4" />}>
            Devices
          </NavLink>
          <NavLink to="/providers" icon={<Server className="size-4" />}>
            Providers
          </NavLink>
          {me?.isAdmin && (
            <>
              <NavLink to="/admin/providers" icon={<Server className="size-4" />}>
                Manage
              </NavLink>
              <NavLink to="/admin/users" icon={<Shield className="size-4" />}>
                Users
              </NavLink>
            </>
          )}

          <div className="flex-1" />

          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" className="rounded-full">
                <Avatar className="size-8">
                  <AvatarImage src={me?.image ?? undefined} alt="" />
                  <AvatarFallback className="text-xs">
                    {(me?.name ?? me?.email ?? "?").slice(0, 2).toUpperCase()}
                  </AvatarFallback>
                </Avatar>
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-56">
              <DropdownMenuLabel className="flex flex-col">
                <span>{me?.name}</span>
                <span className="font-normal text-muted-foreground text-xs">{me?.email}</span>
              </DropdownMenuLabel>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                onSelect={async () => {
                  await authClient.signOut();
                  await router.navigate({ to: "/login", search: {} });
                }}
              >
                <LogOut className="size-4" />
                Sign out
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </header>

      <main className="mx-auto w-full max-w-[1600px] flex-1 px-4 py-6">{children}</main>
    </div>
  );
}

function NavLink({ to, icon, children }: { to: string; icon: ReactNode; children: ReactNode }) {
  return (
    <Link
      to={to}
      className="flex items-center gap-2 rounded-md px-3 py-1.5 font-medium text-muted-foreground text-sm transition-colors hover:bg-accent hover:text-foreground"
      activeProps={{ className: "bg-accent text-foreground" }}
    >
      {icon}
      {children}
    </Link>
  );
}
