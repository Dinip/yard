import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, redirect } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { trpc } from "@/lib/trpc";
import type { RouterInputs } from "@/lib/types";
import { cn, relativeTime } from "@/lib/utils";

type SettingKey = RouterInputs["settings"]["set"]["key"];

export const Route = createFileRoute("/_app/admin/settings")({
  beforeLoad: ({ context }) => {
    // A UX guard, not a security boundary — `adminProcedure` is the real one.
    if (context.user?.role !== "admin") throw redirect({ to: "/devices" });
  },
  component: SettingsPage,
});

function SettingsPage() {
  const qc = useQueryClient();
  const { data } = useQuery(trpc.settings.get.queryOptions());

  const save = useMutation(
    trpc.settings.set.mutationOptions({
      onSuccess: () => {
        toast.success("Saved");
        qc.invalidateQueries({ queryKey: trpc.settings.get.queryKey() });
        qc.invalidateQueries({ queryKey: trpc.settings.public.queryKey() });
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  if (!data) return null;

  const changedAt = (key: SettingKey) => data.changed.find((row) => row.key === key);

  return (
    <div className="flex flex-col gap-6">
      <div>
        <h1 className="font-semibold text-2xl">Settings</h1>
        <p className="text-muted-foreground text-sm">
          Farm-wide policy. Changes take effect within a few seconds — no restart, and no redeploy.
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Reservations</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-6">
          <DurationSetting
            id="ttl"
            label="Reservation lifetime"
            help="How long a reservation survives without the browser renewing it. The tab renews every third of this, so it is the window in which a closed laptop keeps a device."
            minutes={data.values["reservation.ttlSeconds"] / 60}
            pending={save.isPending}
            onSave={(minutes) =>
              minutes !== null &&
              save.mutate({ key: "reservation.ttlSeconds", value: minutes * 60 })
            }
            changed={changedAt("reservation.ttlSeconds")}
          />

          <DurationSetting
            id="idle"
            label="Idle timeout"
            help="Release a device nobody has touched for this long. Interaction counts whether it came through the browser or straight from adb — the provider reports it either way. Empty turns it off."
            minutes={secondsToMinutes(data.values["reservation.idleTimeoutSeconds"])}
            optional
            pending={save.isPending}
            onSave={(minutes) =>
              save.mutate({
                key: "reservation.idleTimeoutSeconds",
                value: minutes === null ? null : minutes * 60,
              })
            }
            changed={changedAt("reservation.idleTimeoutSeconds")}
          />

          <DurationSetting
            id="max"
            label="Maximum session length"
            help="A hard cap on one reservation, however busy it is. Empty turns it off."
            minutes={secondsToMinutes(data.values["reservation.maxDurationSeconds"])}
            optional
            pending={save.isPending}
            onSave={(minutes) =>
              save.mutate({
                key: "reservation.maxDurationSeconds",
                value: minutes === null ? null : minutes * 60,
              })
            }
            changed={changedAt("reservation.maxDurationSeconds")}
          />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Cleanup between users</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-6">
          <ToggleSetting
            id="cleanup-enabled"
            label="Reset devices when a reservation ends"
            help="The device is held out of the pool while its provider resets it, and comes back when that finishes. A provider that dies mid-clean cannot strand a device — it is returned automatically."
            checked={data.values["cleanup.enabled"]}
            pending={save.isPending}
            onChange={(value) => save.mutate({ key: "cleanup.enabled", value })}
            changed={changedAt("cleanup.enabled")}
          />

          {/* The steps stay visible when cleanup is off, greyed rather than
              hidden: an admin turning this on needs to see what they are about
              to switch on before they switch it on. */}
          <div
            className={cn(
              "grid gap-6 border-l pl-4",
              !data.values["cleanup.enabled"] && "pointer-events-none opacity-50",
            )}
          >
            <ToggleSetting
              id="cleanup-uninstall"
              label="Uninstall apps installed during the session"
              help="Anything that appeared since the session started. Apps the device already had are left alone, and if the provider restarted mid-session it declines to guess."
              checked={data.values["cleanup.uninstallApps"]}
              pending={save.isPending}
              onChange={(value) => save.mutate({ key: "cleanup.uninstallApps", value })}
              changed={changedAt("cleanup.uninstallApps")}
            />
            <ToggleSetting
              id="cleanup-screen"
              label="Reset the screen"
              help="Back to the home screen, rotation upright, clipboard cleared."
              checked={data.values["cleanup.resetScreen"]}
              pending={save.isPending}
              onChange={(value) => save.mutate({ key: "cleanup.resetScreen", value })}
              changed={changedAt("cleanup.resetScreen")}
            />
            <ToggleSetting
              id="cleanup-data"
              label="Clear app data"
              help="Wipes the data of every third-party app left on the device — accounts, caches, settings. Android only. Off by default because that list includes anything preinstalled across your fleet, such as a test harness."
              checked={data.values["cleanup.clearAppData"]}
              pending={save.isPending}
              onChange={(value) => save.mutate({ key: "cleanup.clearAppData", value })}
              changed={changedAt("cleanup.clearAppData")}
            />
            <ToggleSetting
              id="cleanup-folders"
              label="Empty scratch folders"
              help="Which folders is set per device in each provider's config, not here — these end in a recursive delete on a phone. A device with none configured skips this."
              checked={data.values["cleanup.wipeFolders"]}
              pending={save.isPending}
              onChange={(value) => save.mutate({ key: "cleanup.wipeFolders", value })}
              changed={changedAt("cleanup.wipeFolders")}
            />

            <SecondsSetting
              id="cleanup-timeout"
              label="Cleanup deadline"
              help="How long a provider may hold a device before giving up and returning it anyway. A partial reset is reported in the audit log."
              seconds={data.values["cleanup.timeoutSeconds"]}
              pending={save.isPending}
              onSave={(seconds) => save.mutate({ key: "cleanup.timeoutSeconds", value: seconds })}
              changed={changedAt("cleanup.timeoutSeconds")}
            />
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function ToggleSetting({
  id,
  label,
  help,
  checked,
  pending,
  onChange,
  changed,
}: {
  id: string;
  label: string;
  help: string;
  checked: boolean;
  pending: boolean;
  onChange: (value: boolean) => void;
  changed?: { updatedAt: string | Date; updatedByName: string | null };
}) {
  return (
    <div className="grid gap-1.5">
      <div className="flex items-center gap-3">
        <Switch id={id} checked={checked} disabled={pending} onCheckedChange={onChange} />
        <Label htmlFor={id}>{label}</Label>
      </div>
      <p className="max-w-2xl text-muted-foreground text-xs">{help}</p>
      <ChangedNote changed={changed} />
    </div>
  );
}

/**
 * A duration in seconds rather than minutes.
 *
 * The reservation policies above are all human-scale — "half an hour" — but a
 * cleanup deadline is a machine one, and rounding it to minutes would make the
 * difference between 90 and 120 seconds unexpressible.
 */
function SecondsSetting({
  id,
  label,
  help,
  seconds,
  pending,
  onSave,
  changed,
}: {
  id: string;
  label: string;
  help: string;
  seconds: number;
  pending: boolean;
  onSave: (seconds: number) => void;
  changed?: { updatedAt: string | Date; updatedByName: string | null };
}) {
  const [draft, setDraft] = useState(String(seconds));

  useEffect(() => {
    setDraft(String(seconds));
  }, [seconds]);

  const parsed = Number(draft);
  const invalid = !Number.isInteger(parsed) || parsed < 10 || parsed > 600;
  const dirty = parsed !== seconds;

  return (
    <div className="grid gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      <div className="flex items-center gap-2">
        <Input
          id={id}
          inputMode="numeric"
          className="w-32"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
        />
        <span className="text-muted-foreground text-sm">seconds</span>
        <Button
          size="sm"
          variant="outline"
          disabled={pending || invalid || !dirty}
          onClick={() => onSave(parsed)}
        >
          Save
        </Button>
      </div>
      <p className="max-w-2xl text-muted-foreground text-xs">{help}</p>
      <ChangedNote changed={changed} />
    </div>
  );
}

function ChangedNote({
  changed,
}: {
  changed?: { updatedAt: string | Date; updatedByName: string | null };
}) {
  if (!changed) return null;
  return (
    <p className="text-muted-foreground text-xs">
      Changed {relativeTime(changed.updatedAt)}
      {changed.updatedByName ? ` by ${changed.updatedByName}` : ""}
    </p>
  );
}

function secondsToMinutes(seconds: number | null) {
  return seconds === null ? null : seconds / 60;
}

/**
 * One duration, in minutes.
 *
 * Minutes rather than seconds because every one of these is a human-scale
 * policy — "half an hour", not "1800" — and the wire keeps seconds so the
 * server never has to guess at a unit.
 */
function DurationSetting({
  id,
  label,
  help,
  minutes,
  optional,
  pending,
  onSave,
  changed,
}: {
  id: string;
  label: string;
  help: string;
  minutes: number | null;
  optional?: boolean;
  pending: boolean;
  onSave: (minutes: number | null) => void;
  changed?: { updatedAt: string | Date; updatedByName: string | null };
}) {
  const [draft, setDraft] = useState(minutes === null ? "" : String(minutes));

  // The saved value is the source of truth: a refetch (or another admin's
  // change) must not be masked by a stale draft nobody submitted.
  useEffect(() => {
    setDraft(minutes === null ? "" : String(minutes));
  }, [minutes]);

  const parsed = draft.trim() === "" ? null : Number(draft);
  const invalid = parsed === null ? !optional : !Number.isFinite(parsed) || parsed <= 0;
  const dirty = parsed !== minutes;

  return (
    <div className="grid gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      <div className="flex items-center gap-2">
        <Input
          id={id}
          inputMode="numeric"
          className="w-32"
          placeholder={optional ? "off" : undefined}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
        />
        <span className="text-muted-foreground text-sm">minutes</span>
        <Button
          size="sm"
          variant="outline"
          disabled={pending || invalid || !dirty}
          onClick={() => onSave(parsed)}
        >
          Save
        </Button>
      </div>
      <p className="max-w-2xl text-muted-foreground text-xs">{help}</p>
      <ChangedNote changed={changed} />
    </div>
  );
}
