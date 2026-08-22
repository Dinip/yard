import { QueryClient } from "@tanstack/react-query";
import { createTRPCClient, httpBatchLink, httpSubscriptionLink, splitLink } from "@trpc/client";
import { createTRPCOptionsProxy } from "@trpc/tanstack-react-query";
import type { AppRouter } from "@yard/coordinator/router";

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 5_000,
      retry: (failureCount, error) => {
        // Never retry an auth failure into a redirect loop.
        const code = (error as { data?: { code?: string } })?.data?.code;
        if (code === "UNAUTHORIZED" || code === "FORBIDDEN") return false;
        return failureCount < 2;
      },
    },
  },
});

export const trpcClient = createTRPCClient<AppRouter>({
  links: [
    splitLink({
      // Subscriptions ride SSE, which cannot be batched and must stay open.
      // Everything else batches over a single POST.
      condition: (op) => op.type === "subscription",
      true: httpSubscriptionLink({
        url: "/api/trpc",
        // EventSource cannot set headers, but the app is single-origin (Vite
        // proxies /api in dev, Caddy in prod) so the session cookie rides along.
        eventSourceOptions: { withCredentials: true },
      }),
      false: httpBatchLink({
        url: "/api/trpc",
        fetch: (url, opts) => fetch(url, { ...opts, credentials: "include" }),
      }),
    }),
  ],
});

export const trpc = createTRPCOptionsProxy<AppRouter>({ client: trpcClient, queryClient });
