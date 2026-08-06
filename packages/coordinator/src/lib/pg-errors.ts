/**
 * Postgres unique-violation, wherever it ended up in the error chain.
 *
 * Reservation exclusivity is a database property — the partial unique index on
 * `reservation(device_id) where state = 'active'` — and this is the function
 * that turns losing that race into a `CONFLICT` the user understands. It used
 * to read `err.code` directly, which stopped matching when drizzle began
 * wrapping failures in a `DrizzleQueryError` that carries the driver's error as
 * its `cause`: the loser got a 500 and a stack trace containing the whole
 * INSERT instead of "Device is in use".
 *
 * Walking the chain rather than reaching for `.cause` once, so another layer of
 * wrapping cannot quietly break it again.
 */
export function isUniqueViolation(err: unknown): boolean {
  for (let current = err, depth = 0; current && depth < 5; depth++) {
    if (typeof current === "object" && "code" in current && current.code === "23505") return true;
    current = (current as { cause?: unknown }).cause;
  }
  return false;
}
