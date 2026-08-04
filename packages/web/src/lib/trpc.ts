import type { AppRouter } from "@farm/coordinator/router";
import { QueryClient } from "@tanstack/react-query";
import { createTRPCClient, httpBatchLink } from "@trpc/client";
import { createTRPCOptionsProxy } from "@trpc/tanstack-react-query";

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
    httpBatchLink({
      url: "/api/trpc",
      fetch: (url, opts) => fetch(url, { ...opts, credentials: "include" }),
    }),
  ],
});

export const trpc = createTRPCOptionsProxy<AppRouter>({ client: trpcClient, queryClient });
