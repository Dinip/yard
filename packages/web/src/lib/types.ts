import type { AppRouter } from "@farm/coordinator/router";
import type { inferRouterInputs, inferRouterOutputs } from "@trpc/server";

export type RouterInputs = inferRouterInputs<AppRouter>;
export type RouterOutputs = inferRouterOutputs<AppRouter>;

export type DeviceListItem = RouterOutputs["device"]["list"][number];
export type ProviderListItem = RouterOutputs["provider"]["list"][number];
export type AdminUser = RouterOutputs["admin"]["users"]["users"][number];
export type AuditEntry = RouterOutputs["admin"]["audit"][number];
