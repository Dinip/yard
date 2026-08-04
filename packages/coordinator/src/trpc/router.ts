import { router } from "./init.ts";
import { adminRouter } from "./routers/admin.ts";
import { deviceRouter } from "./routers/device.ts";
import { providerRouter } from "./routers/provider.ts";
import { userRouter } from "./routers/user.ts";

export const appRouter = router({
  user: userRouter,
  device: deviceRouter,
  provider: providerRouter,
  admin: adminRouter,
});

export type AppRouter = typeof appRouter;
