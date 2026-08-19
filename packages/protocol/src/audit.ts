/**
 * Every action the coordinator writes to the audit log.
 *
 * These were scattered string literals across four routers, and the admin UI
 * kept a hand-maintained copy of the subset it knew about — which had already
 * drifted twice, silently, because a filter option that does not exist looks
 * exactly like an action that never happened.
 *
 * It lives in `packages/protocol` rather than the coordinator because the web
 * app needs the *values*, and the working agreement is that `packages/web`
 * imports coordinator **types** only. Nothing here is a wire schema: it is
 * deliberately a plain const rather than a `named()` zod object, so the Rust
 * generator does not emit it — a provider has no notion of audit actions.
 *
 * The label is what an admin reads in a filter; the value is what is in the
 * column. Adding an action means adding it here, and the coordinator will not
 * compile if it is written any other way.
 */
export const AUDIT_ACTIONS = [
  // Reservations — the lifecycle of a device changing hands.
  { value: "device.reserve", label: "Reserved", group: "Reservations" },
  { value: "device.release", label: "Released", group: "Reservations" },
  { value: "device.force_release", label: "Force released", group: "Reservations" },
  { value: "device.reservation_expired", label: "Expired", group: "Reservations" },
  { value: "device.reservation_idle", label: "Released when idle", group: "Reservations" },
  {
    value: "device.reservation_max_duration",
    label: "Released at the session limit",
    group: "Reservations",
  },

  // Sessions — somebody present in someone else's.
  { value: "device.session_join", label: "Joined a session", group: "Sessions" },
  { value: "device.session_leave", label: "Left a session", group: "Sessions" },
  { value: "device.session_request", label: "Asked to join", group: "Sessions" },
  { value: "device.session_request_approved", label: "Join approved", group: "Sessions" },
  { value: "device.session_request_denied", label: "Join denied", group: "Sessions" },

  // Things done to a device.
  { value: "device.install", label: "Installs", group: "Devices" },
  { value: "device.uninstall", label: "Uninstalls", group: "Devices" },
  { value: "device.launch", label: "App launches", group: "Devices" },
  { value: "device.reboot", label: "Reboots", group: "Devices" },
  { value: "device.cleanup", label: "Cleaned between users", group: "Devices" },
  { value: "device.restart", label: "Session restarts", group: "Devices" },
  { value: "device.adb.expose", label: "Remote debugging enabled", group: "Devices" },
  { value: "device.file_pull", label: "Pulled a file", group: "Devices" },
  { value: "device.adb.key_approved", label: "adb key approved", group: "Devices" },
  { value: "device.adb.key_denied", label: "adb key denied", group: "Devices" },

  // A user's own ADB keys. Not "Devices": these are account-level and outlive
  // any one session, which is the whole point of registering one.
  { value: "user.adb_key.add", label: "adb key added", group: "Administration" },
  { value: "user.adb_key.remove", label: "adb key removed", group: "Administration" },

  // Administration.
  { value: "provider.create", label: "Provider created", group: "Administration" },
  { value: "provider.update", label: "Provider updated", group: "Administration" },
  { value: "provider.delete", label: "Provider deleted", group: "Administration" },
  { value: "provider.token.create", label: "Token issued", group: "Administration" },
  { value: "provider.token.revoke", label: "Token revoked", group: "Administration" },
  { value: "settings.update", label: "Setting changed", group: "Administration" },
] as const;

export type AuditAction = (typeof AUDIT_ACTIONS)[number]["value"];
export type AuditActionGroup = (typeof AUDIT_ACTIONS)[number]["group"];

export const AUDIT_ACTION_VALUES = AUDIT_ACTIONS.map((a) => a.value) as [
  AuditAction,
  ...AuditAction[],
];

/**
 * The readable name for an action, falling back to the raw value.
 *
 * A row written by an older version of the coordinator must still render: the
 * audit log is a historical record, and an action retired from this list has
 * not stopped having happened.
 */
export function auditActionLabel(value: string): string {
  return AUDIT_ACTIONS.find((a) => a.value === value)?.label ?? value;
}

export type AuditActionEntry = (typeof AUDIT_ACTIONS)[number];

/** Actions bucketed for a grouped `<Select>`, in declaration order. */
export function auditActionsByGroup(): { group: AuditActionGroup; actions: AuditActionEntry[] }[] {
  const out: { group: AuditActionGroup; actions: AuditActionEntry[] }[] = [];
  for (const action of AUDIT_ACTIONS) {
    const bucket = out.find((b) => b.group === action.group);
    if (bucket) bucket.actions.push(action);
    else out.push({ group: action.group, actions: [action] });
  }
  return out;
}
