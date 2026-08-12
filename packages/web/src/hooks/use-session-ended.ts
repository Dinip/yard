import { type RefObject, useCallback, useEffect, useRef, useState } from "react";

/**
 * Notice that a session has ended, from either of the two things that can say
 * so.
 *
 * The provider revokes on the session plane, which carries a reason — and needs
 * a live socket to arrive at all, so a reservation reaped after the stream
 * dropped, or never started, announces nothing. The inventory is the other
 * half: a session that is no longer in `device.get` has ended whatever the
 * socket did. Neither alone is enough, and both may fire, so the first answer
 * carrying a reason is the one kept.
 */
export function useSessionEnded(
  inSession: boolean,
  /** While true, endings are the user's own doing and go unreported. */
  ignore?: RefObject<boolean>,
) {
  const [ended, setEnded] = useState<{ reason?: string } | null>(null);

  const reportEnded = useCallback(
    (reason?: string) => {
      if (ignore?.current) return;
      setEnded((previous) => (previous?.reason ? previous : { reason }));
    },
    [ignore],
  );

  const hadSession = useRef(false);
  useEffect(() => {
    if (inSession) hadSession.current = true;
    else if (hadSession.current) reportEnded();
  }, [inSession, reportEnded]);

  return { ended, reportEnded };
}
