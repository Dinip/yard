import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Trash2 } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { trpc } from "@/lib/trpc";
import { relativeTime } from "@/lib/utils";

export const Route = createFileRoute("/_app/settings")({
  component: SettingsPage,
});

function SettingsPage() {
  return (
    <div className="grid gap-6">
      <div>
        <h1 className="font-semibold text-xl">Your settings</h1>
        <p className="text-muted-foreground text-sm">Settings that apply to your account only.</p>
      </div>
      <AdbKeys />
    </div>
  );
}

/**
 * A key registered here is what makes `adb connect` silent.
 *
 * Without one, the first connect parks and asks whoever holds the device to
 * approve it — which works, but means bothering somebody and waiting on them.
 * The phone's own opinion of the key is no longer involved either way.
 */
function AdbKeys() {
  const qc = useQueryClient();
  const { data: keys } = useQuery(trpc.user.adbKeys.list.queryOptions());
  const [publicKey, setPublicKey] = useState("");
  const [title, setTitle] = useState("");

  const refresh = () => qc.invalidateQueries({ queryKey: trpc.user.adbKeys.list.queryKey() });

  const add = useMutation(
    trpc.user.adbKeys.add.mutationOptions({
      onSuccess: () => {
        toast.success("Key added");
        setPublicKey("");
        setTitle("");
        refresh();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  const remove = useMutation(
    trpc.user.adbKeys.remove.mutationOptions({
      onSuccess: () => {
        toast.success("Key removed");
        refresh();
      },
      onError: (e) => toast.error(e.message),
    }),
  );

  return (
    <section className="grid gap-4 rounded-lg border p-4">
      <div>
        <h2 className="font-medium">adb keys</h2>
        <p className="text-muted-foreground text-sm">
          Registering your key lets you <code className="font-mono">adb connect</code> to any device
          you have reserved without asking anyone. It identifies you: commands run through it are
          recorded against your account.
        </p>
      </div>

      {keys && keys.length > 0 && (
        <ul className="grid gap-2">
          {keys.map((key) => (
            <li
              key={key.id}
              className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded border px-3 py-2"
            >
              <span className="font-medium text-sm">{key.title}</span>
              <code className="truncate font-mono text-muted-foreground text-xs">
                {key.fingerprint}
              </code>
              {key.comment && <span className="text-muted-foreground text-xs">{key.comment}</span>}
              <div className="flex-1" />
              <span className="text-muted-foreground text-xs">
                {/* Last used, not added: the question people actually have
                    about an old key is whether anything still relies on it. */}
                {key.lastUsedAt ? `last used ${relativeTime(key.lastUsedAt)}` : "never used"}
              </span>
              <Button
                variant="ghost"
                size="icon"
                aria-label={`Remove ${key.title}`}
                disabled={remove.isPending}
                onClick={() => remove.mutate({ id: key.id })}
              >
                <Trash2 className="size-4" />
              </Button>
            </li>
          ))}
        </ul>
      )}

      <form
        className="grid gap-3"
        onSubmit={(event) => {
          event.preventDefault();
          add.mutate({ publicKey, title });
        }}
      >
        <div className="grid gap-1.5">
          <Label htmlFor="adb-key">Public key</Label>
          {/* Most people have never opened this file, so the command to print
              it is part of the instruction rather than something to look up. */}
          <p className="text-muted-foreground text-xs">
            Run <code className="font-mono">cat ~/.android/adbkey.pub</code> and paste the whole
            line. That is the public half — never paste <code className="font-mono">adbkey</code>{" "}
            itself.
          </p>
          <Textarea
            id="adb-key"
            required
            rows={3}
            spellCheck={false}
            className="font-mono text-xs"
            placeholder="QAAAA… user@host"
            value={publicKey}
            onChange={(event) => setPublicKey(event.target.value)}
          />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="adb-key-title">Name</Label>
          <Input
            id="adb-key-title"
            required
            maxLength={100}
            placeholder="Work laptop"
            value={title}
            onChange={(event) => setTitle(event.target.value)}
          />
        </div>
        <div>
          <Button type="submit" disabled={add.isPending || !publicKey.trim() || !title.trim()}>
            Add key
          </Button>
        </div>
      </form>
    </section>
  );
}
