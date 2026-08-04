import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute, redirect, useRouter } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { z } from "zod";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
import { authClient } from "@/lib/auth-client";
import { trpc } from "@/lib/trpc";

export const Route = createFileRoute("/login")({
  validateSearch: z.object({ redirect: z.string().optional() }),
  beforeLoad: async ({ search }) => {
    const { data } = await authClient.getSession();
    if (data?.session) throw redirect({ to: search.redirect ?? "/devices" });
  },
  component: LoginPage,
});

function LoginPage() {
  const router = useRouter();
  const { redirect: redirectTo } = Route.useSearch();
  const { data: caps } = useQuery(trpc.user.capabilities.queryOptions());
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [mode, setMode] = useState<"signIn" | "signUp">("signIn");

  const microsoft = useMutation({
    mutationFn: () =>
      authClient.signIn.social({
        provider: "microsoft",
        callbackURL: redirectTo ?? "/devices",
      }),
  });

  const credentials = useMutation({
    mutationFn: async () => {
      const fn =
        mode === "signIn"
          ? authClient.signIn.email({ email, password })
          : authClient.signUp.email({ email, password, name: email.split("@")[0] ?? email });
      const { error } = await fn;
      if (error) throw new Error(error.message ?? "Authentication failed");
    },
    onSuccess: async () => {
      await router.invalidate();
      await router.navigate({ to: redirectTo ?? "/devices" });
    },
    onError: (err: Error) => toast.error(err.message),
  });

  return (
    <div className="flex min-h-svh items-center justify-center bg-background p-6">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle className="text-2xl">{caps?.appName ?? "Device Farm"}</CardTitle>
          <CardDescription>Sign in to reserve and control devices.</CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          {caps?.microsoft && (
            <Button
              variant="outline"
              className="w-full"
              disabled={microsoft.isPending}
              onClick={() => microsoft.mutate()}
            >
              <MicrosoftLogo />
              Continue with Microsoft
            </Button>
          )}

          {caps?.microsoft && caps?.emailPassword && (
            <div className="relative">
              <Separator />
              <span className="-translate-x-1/2 -translate-y-1/2 absolute top-1/2 left-1/2 bg-card px-2 text-muted-foreground text-xs">
                or
              </span>
            </div>
          )}

          {caps?.emailPassword && (
            <form
              className="flex flex-col gap-3"
              onSubmit={(e) => {
                e.preventDefault();
                credentials.mutate();
              }}
            >
              <div className="grid gap-1.5">
                <Label htmlFor="email">Email</Label>
                <Input
                  id="email"
                  type="email"
                  autoComplete="email"
                  required
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="password">Password</Label>
                <Input
                  id="password"
                  type="password"
                  autoComplete={mode === "signIn" ? "current-password" : "new-password"}
                  required
                  minLength={8}
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </div>
              <Button type="submit" disabled={credentials.isPending}>
                {mode === "signIn" ? "Sign in" : "Create account"}
              </Button>
              <button
                type="button"
                className="text-muted-foreground text-xs hover:underline"
                onClick={() => setMode(mode === "signIn" ? "signUp" : "signIn")}
              >
                {mode === "signIn" ? "Create an account" : "I already have an account"}
              </button>
            </form>
          )}

          {caps && !caps.microsoft && !caps.emailPassword && (
            <p className="text-destructive text-sm">
              No sign-in method is configured. Set MICROSOFT_CLIENT_ID/SECRET or
              ENABLE_EMAIL_PASSWORD on the coordinator.
            </p>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function MicrosoftLogo() {
  return (
    <svg viewBox="0 0 23 23" aria-hidden="true" className="size-4">
      <path fill="#f35325" d="M1 1h10v10H1z" />
      <path fill="#81bc06" d="M12 1h10v10H12z" />
      <path fill="#05a6f0" d="M1 12h10v10H1z" />
      <path fill="#ffba08" d="M12 12h10v10H12z" />
    </svg>
  );
}
