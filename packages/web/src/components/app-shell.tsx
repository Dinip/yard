import { useQuery } from "@tanstack/react-query";
import { Link, useRouter } from "@tanstack/react-router";
import {
  Boxes,
  LogOut,
  type LucideIcon,
  ScrollText,
  Server,
  Shield,
  SlidersHorizontal,
  Smartphone,
} from "lucide-react";
import type { ReactNode } from "react";
import { BuildStamp, GithubMark } from "@/components/build-stamp";
import { ThemeToggle } from "@/components/theme-toggle";
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
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { authClient } from "@/lib/auth-client";
import { REPO_URL } from "@/lib/build-info";
import { trpc } from "@/lib/trpc";

/**
 * Navigation down the left edge, not across the top.
 *
 * A top bar spent a whole row of every page on, for a non-admin, one link — and
 * the device page then put its own header underneath it, so two rows of chrome
 * sat above a picture that wanted the height. Vertically the same navigation
 * costs 56px of width, which is the axis a portrait device leaves spare.
 */
export function AppShell({ children }: { children: ReactNode }) {
  const router = useRouter();
  const { data: me } = useQuery(trpc.user.me.queryOptions());
  // The product name is hardcoded on the coordinator and reaches the browser
  // through `user.capabilities`, so it has one source instead of two.
  const { data: caps } = useQuery(trpc.user.capabilities.queryOptions());

  return (
    <TooltipProvider delayDuration={300}>
      <div className="flex h-svh">
        <nav
          aria-label="Main"
          className="flex w-14 shrink-0 flex-col items-center gap-1 border-r bg-muted/30 py-3"
        >
          {/* The only place the product name appears in the chrome now, so it
              is a tooltip rather than nothing. The mark is deliberately not the
              Devices glyph — the same icon twice in one column reads as a bug. */}
          <Hint label={caps?.appName ?? ""} side="right">
            <Link
              to="/devices"
              aria-label={caps?.appName}
              className="mb-2 flex size-9 items-center justify-center rounded-lg bg-primary text-primary-foreground"
            >
              <Boxes className="size-5" />
            </Link>
          </Hint>

          <NavLink to="/devices" icon={Smartphone} label="Devices" />
          {me?.isAdmin && (
            <>
              <NavLink to="/admin/providers" icon={Server} label="Providers" />
              <NavLink to="/admin/users" icon={Shield} label="Users" />
              <NavLink to="/admin/audit" icon={ScrollText} label="Audit" />
              <NavLink to="/admin/settings" icon={SlidersHorizontal} label="Settings" />
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
            <DropdownMenuContent side="right" align="end" className="w-56">
              <DropdownMenuLabel className="flex flex-col">
                <span>{me?.name}</span>
                <span className="font-normal text-muted-foreground text-xs">{me?.email}</span>
              </DropdownMenuLabel>
              <DropdownMenuSeparator />
              {/* In the account menu rather than the rail: these are yours,
                  not the farm's, and the rail is for places you go often. */}
              <DropdownMenuItem asChild>
                <Link to="/settings">
                  <SlidersHorizontal className="size-4" />
                  Your settings
                </Link>
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <ThemeToggle />
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
              <DropdownMenuSeparator />
              {/* A footnote, not a menu item: nobody opens this menu to visit
                  the repository, so the link is the mark beside the build it
                  points at. */}
              <div className="flex items-center justify-between px-2 py-1.5">
                <BuildStamp />
                <a
                  href={REPO_URL}
                  target="_blank"
                  rel="noreferrer"
                  className="text-muted-foreground hover:text-foreground"
                  aria-label="Source on GitHub"
                >
                  <GithubMark className="size-3.5" />
                </a>
              </div>
            </DropdownMenuContent>
          </DropdownMenu>
        </nav>

        {/* The scroll container is the full-width element, so the scrollbar
            stays at the window edge and the padding belongs to the scrolled
            area; the centred column is inside it.

            `min-h-full` on that column: at least the viewport, so a page can
            fill it with `flex-1`, but free to grow so a long page's last row is
            not pushed past this element's own padding-bottom. That only works
            because nothing inside claims a large intrinsic height — see the
            note in `DeviceScreen`, which lays the picture out of flow for
            exactly this reason. */}
        <main className="min-h-0 min-w-0 flex-1 overflow-y-auto px-6 py-4">
          <div className="mx-auto flex min-h-full w-full max-w-[1600px] flex-col">{children}</div>
        </main>
      </div>
    </TooltipProvider>
  );
}

function NavLink({ to, icon: Icon, label }: { to: string; icon: LucideIcon; label: string }) {
  return (
    <Hint label={label} side="right">
      <Link
        to={to}
        aria-label={label}
        className="flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        activeProps={{ className: "bg-accent text-foreground" }}
      >
        <Icon className="size-4" />
      </Link>
    </Hint>
  );
}

/** The label an icon-only rail cannot show. */
function Hint({ label, side, children }: { label: string; side: "right"; children: ReactNode }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent side={side}>{label}</TooltipContent>
    </Tooltip>
  );
}
