import { router } from "./init.ts";
import { adminRouter } from "./routers/admin.ts";
import { deviceRouter } from "./routers/device.ts";
import { providerRouter } from "./routers/provider.ts";
import { streamRouter } from "./routers/stream.ts";
import { userRouter } from "./routers/user.ts";

export const appRouter = router({
  user: userRouter,
  device: deviceRouter,
  provider: providerRouter,
  admin: adminRouter,
  stream: streamRouter,
});

export type AppRouter = typeof appRouter;
