import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  listRuntimes,
  listSchedules,
  addSchedule,
  removeSchedule,
  setScheduleEnabled,
  type ScheduleTrigger,
} from "@/lib/ipc";
import { Button, Section, Switch, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";

function triggerSummary(t: ScheduleTrigger): string {
  if (t.kind === "interval") {
    const s = t.every_secs;
    if (s % 3600 === 0) return `every ${s / 3600} h`;
    if (s % 60 === 0) return `every ${s / 60} min`;
    return `every ${s}s`;
  }
  return `daily at ${String(t.hour).padStart(2, "0")}:${String(t.minute).padStart(2, "0")} UTC`;
}

// Scheduled / triggered agents: each fires a prompt on a recurring schedule
// into a fresh chat. Times are UTC (interval triggers are tz-independent).
export function SchedulesSection() {
  const qc = useQueryClient();
  const schedules = useQuery({ queryKey: ["schedules"], queryFn: listSchedules });
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });

  const [label, setLabel] = useState("");
  const [prompt, setPrompt] = useState("");
  const [runtimeId, setRuntimeId] = useState("");
  const [mode, setMode] = useState<"interval" | "daily_at">("interval");
  const [everyMin, setEveryMin] = useState("60");
  const [atTime, setAtTime] = useState("09:00");

  const refresh = () => qc.invalidateQueries({ queryKey: ["schedules"] });
  const remove = useMutation({ mutationFn: removeSchedule, onSuccess: refresh });
  const toggle = useMutation({
    mutationFn: (v: { id: string; enabled: boolean }) => setScheduleEnabled(v.id, v.enabled),
    onSuccess: refresh,
  });
  const add = useMutation({
    mutationFn: () => {
      let trigger: ScheduleTrigger;
      if (mode === "interval") {
        const mins = Math.max(1, Math.round(Number(everyMin) || 0));
        trigger = { kind: "interval", every_secs: mins * 60 };
      } else {
        const [h, m] = atTime.split(":").map((n) => parseInt(n, 10));
        trigger = { kind: "daily_at", hour: h || 0, minute: m || 0 };
      }
      return addSchedule({ label, prompt, runtimeId: runtimeId || undefined, trigger });
    },
    onSuccess: () => {
      setLabel("");
      setPrompt("");
      refresh();
    },
    onError: (e) => toast.error(`Couldn't add schedule: ${errMsg(e)}`),
  });

  return (
    <Section title="Schedules">
      <p className="text-sm opacity-60">
        Run an agent on a recurring schedule — each fire opens a new chat, posts the prompt, and the
        agent answers. Times are UTC.
      </p>

      {(schedules.data ?? []).map((s) => (
        <div
          key={s.id}
          className="flex items-start justify-between gap-3 rounded-lg border p-3"
          style={{ borderColor: "var(--hive-line)" }}
        >
          <div className="min-w-0">
            <div className="font-medium">{s.label || "Scheduled run"}</div>
            <div className="text-xs opacity-60">{triggerSummary(s.trigger)}</div>
            <div className="mt-1 truncate text-sm opacity-75">{s.prompt}</div>
          </div>
          <div className="flex shrink-0 items-center gap-3">
            <Switch
              on={s.enabled}
              onChange={(v) => toggle.mutate({ id: s.id, enabled: v })}
              label={`Enable schedule "${s.label || "Scheduled run"}"`}
            />
            <button
              className="text-xs hover:opacity-80"
              style={{ color: "var(--hive-danger)" }}
              onClick={() => confirmThen(`Remove schedule "${s.label || "Scheduled run"}"?`, () => remove.mutate(s.id))}
            >
              Remove
            </button>
          </div>
        </div>
      ))}
      {(schedules.data ?? []).length === 0 && <p className="text-sm opacity-50">No schedules yet.</p>}

      {/* Add form */}
      <div className="mt-2 space-y-2 rounded-lg border p-3" style={{ borderColor: "var(--hive-line)" }}>
        <div className="text-sm font-medium">New schedule</div>
        <input
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          placeholder="Label (e.g. Daily standup digest)"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={fieldStyle}
        />
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          placeholder="Prompt to send each run…"
          rows={2}
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={fieldStyle}
        />
        <div className="flex flex-wrap items-center gap-2">
          <select
            value={mode}
            onChange={(e) => setMode(e.target.value as "interval" | "daily_at")}
            className="rounded-xl border px-3 py-2 text-sm"
            style={fieldStyle}
          >
            <option value="interval">Every</option>
            <option value="daily_at">Daily at</option>
          </select>
          {mode === "interval" ? (
            <>
              <input
                type="number"
                min={1}
                value={everyMin}
                onChange={(e) => setEveryMin(e.target.value)}
                className="w-20 rounded-xl border px-2 py-1.5 text-sm"
                style={fieldStyle}
              />
              <span className="text-sm opacity-70">minutes</span>
            </>
          ) : (
            <input
              type="time"
              value={atTime}
              onChange={(e) => setAtTime(e.target.value)}
              className="rounded-xl border px-3 py-2 text-sm"
              style={fieldStyle}
            />
          )}
          <select
            value={runtimeId}
            onChange={(e) => setRuntimeId(e.target.value)}
            className="rounded-xl border px-3 py-2 text-sm"
            style={fieldStyle}
          >
            <option value="">Default runtime</option>
            {(runtimes.data ?? []).map((r) => (
              <option key={r.id} value={r.id}>
                {r.label}
              </option>
            ))}
          </select>
          <Button variant="primary" disabled={!prompt.trim() || add.isPending} onClick={() => add.mutate()}>
            Add schedule
          </Button>
        </div>
      </div>
    </Section>
  );
}
