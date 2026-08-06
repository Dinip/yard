import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, redirect } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { trpc } from "@/lib/trpc";
import type { RouterInputs } from "@/lib/types";
import { relativeTime } from "@/lib/utils";

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
    </div>
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
      {changed && (
        <p className="text-muted-foreground text-xs">
          Changed {relativeTime(changed.updatedAt)}
          {changed.updatedByName ? ` by ${changed.updatedByName}` : ""}
        </p>
      )}
    </div>
  );
}
