import type { RuntimeSummaryDto } from "@/bindings/RuntimeSummaryDto";

export interface QuickTaskOption {
  /// Runtime id, or "" for "Same as chat".
  value: string;
  label: string;
}

/// Options for the "Quick tasks model" picker: "Same as chat" first, then every
/// runtime. A saved id that's no longer listed (runtime removed) stays visible
/// as "(missing)" so the select doesn't silently show the wrong choice — the
/// backend already falls back to the chat's runtime for it.
export function quickTaskOptions(
  runtimes: Pick<RuntimeSummaryDto, "id" | "label">[],
  current: string | null | undefined,
): QuickTaskOption[] {
  const opts: QuickTaskOption[] = [{ value: "", label: "Same as chat" }];
  for (const r of runtimes) opts.push({ value: r.id, label: r.label || r.id });
  if (current && !runtimes.some((r) => r.id === current)) {
    opts.push({ value: current, label: `${current} (missing)` });
  }
  return opts;
}
