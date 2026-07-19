import { memo, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  exportChat,
  getAppSettings,
  getChat,
  listAgents,
  listMembers,
  onChatStream,
  presenceList,
  presencePing,
  saveAttachment,
  readWorkspaceFile,
  sendMessage,
  regenerate,
  summarizeChat,
  compactChat,
  linearIssuesContext,
  setChatRuntime,
  syncStatus,
  toggleReaction,
  type ChatMessageDto,
  type RuntimeSummaryDto,
} from "@/lib/ipc";
import { attachmentMarker, splitAttachments, fileBaseName, isImagePath } from "@/lib/attachments";
import {
  IconCopy,
  IconRegenerate,
  IconSmile,
  IconSend,
  IconPaperclip,
  IconFile,
  IconImage,
  IconInfo,
  IconWrench,
  IconArrowDown,
  IconMessage,
  IconX,
  IconChevronDown,
  IconCheck,
} from "@/lib/icons";
import { toast, errMsg } from "@/components/Toast";
import { SkeletonBubbles } from "@/components/Skeleton";
import { Markdown } from "@/components/Markdown";
import { detectMention, filterMentions } from "@/lib/mentions";
import { applyStreamDelta, retireStream } from "@/lib/streams";
import { detectSlash } from "@/lib/slash";
import { confirmThen } from "@/lib/confirm";
import { promptDialog } from "@/components/Dialog";
import { loadTemplates } from "@/lib/templates";

const QUICK_EMOJI = ["👍", "👎", "🎉", "👀", "❤️"];
const CLAUDE_NOTE_KEY = "hive.claudeCodeNoteDismissed";

// Clickable starter prompts shown in an empty chat — concrete examples of what
// to do, spanning understand / change / git / docs. Clicking fills the composer.
const STARTER_PROMPTS = [
  "Explain what this project does and how it's structured.",
  "Find and fix the failing tests.",
  "Summarize the changes on the current git branch.",
  "Add a setup section to the README.",
];

function runtimePickerLabel(runtime: RuntimeSummaryDto) {
  const provider = runtime.provider.trim();
  const model = runtime.model.trim();
  if (provider && model) return `${provider} / ${model}`;
  if (provider) return provider;
  if (model) return model;
  return runtime.label.trim() || runtime.name.trim() || "runtime";
}

export function ChatView({
  sessionId,
  runtimes,
  currentRuntimeId,
  onOpenTools,
  embedded = false,
}: {
  sessionId: string;
  runtimes: RuntimeSummaryDto[];
  currentRuntimeId: string;
  onOpenTools?: () => void;
  embedded?: boolean;
}) {
  const qc = useQueryClient();
  const [input, setInput] = useState("");
  const [claudeNoteDismissed, setClaudeNoteDismissed] = useState(
    () => typeof window !== "undefined" && window.localStorage.getItem(CLAUDE_NOTE_KEY) === "1",
  );
  const [sending, setSending] = useState(false);
  const [optimisticUser, setOptimisticUser] = useState<string | null>(null);
  // messageId → accumulated text. A Map (not a single slot) because workflow
  // fan-out streams several assistant messages into the chat concurrently.
  const [streams, setStreams] = useState<Map<string, string>>(new Map());
  // Autoscroll only when already pinned to the bottom; otherwise surface a pill.
  const [atBottom, setAtBottom] = useState(true);
  const [hasNew, setHasNew] = useState(false);
  // @-mention autocomplete in the composer.
  const [mention, setMention] = useState<{ start: number; query: string } | null>(null);
  const [mentionActive, setMentionActive] = useState(0);
  // /-slash command menu in the composer.
  const [slash, setSlash] = useState<{ query: string } | null>(null);
  const [slashActive, setSlashActive] = useState(0);
  // Pending composer attachments (saved to disk; referenced by path on send).
  const [attachments, setAttachments] = useState<{ name: string; path: string; image: boolean }[]>([]);
  const sessionRef = useRef(sessionId);
  sessionRef.current = sessionId;
  const scrollRef = useRef<HTMLDivElement>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const lastTypingPing = useRef(0);
  const stopTypingTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const chat = useQuery({
    queryKey: ["chat", sessionId],
    queryFn: () => getChat(sessionId),
  });

  // The local user's display name — the same author the backend stamps on a
  // persisted message. Used to label the optimistic bubble so it doesn't flip
  // from "You" to the display name once the message lands.
  const appSettings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
  const selfName = appSettings.data?.displayName?.trim() || "You";

  // Live presence (other users' typing + online), polled while the chat is open
  // — but only when a relay is configured. Solo/relay-less workspaces skip the
  // poll entirely so the IPC bridge isn't churned (Windows responsiveness).
  const sync = useQuery({ queryKey: ["sync-status"], queryFn: syncStatus });
  const relayConfigured = Boolean(sync.data?.relayConfigured);
  const presence = useQuery({
    queryKey: ["presence", sessionId],
    queryFn: presenceList,
    enabled: relayConfigured,
    refetchInterval: relayConfigured ? 3000 : false,
  });
  const typingNames = (presence.data ?? [])
    .filter((p) => p.typing && p.sessionId === sessionId)
    .map((p) => p.name || "Someone");

  // Heartbeat "typing" (throttled), then auto-clear shortly after the last key.
  function pingTyping() {
    if (!relayConfigured) return;
    const now = Date.now();
    if (now - lastTypingPing.current > 1500) {
      lastTypingPing.current = now;
      void presencePing(sessionId, true).catch(() => {});
    }
    if (stopTypingTimer.current) clearTimeout(stopTypingTimer.current);
    stopTypingTimer.current = setTimeout(() => {
      void presencePing(sessionId, false).catch(() => {});
    }, 3000);
  }
  function clearTyping() {
    if (stopTypingTimer.current) clearTimeout(stopTypingTimer.current);
    lastTypingPing.current = 0;
    if (!relayConfigured) return;
    void presencePing(sessionId, false).catch(() => {});
  }

  // Stable across renders so memoized bubbles don't re-render on every keystroke
  // / stream token (the transcript otherwise re-rendered with the composer).
  const handleReact = useCallback(
    (messageId: string, emoji: string) => {
      void toggleReaction(sessionId, messageId, emoji).then(() =>
        qc.invalidateQueries({ queryKey: ["chat", sessionId] }),
      );
    },
    [sessionId, qc],
  );

  useEffect(() => {
    const unlisten = onChatStream((e) => {
      if (e.sessionId !== sessionRef.current) return;
      if (e.phase === "delta") {
        setStreams((prev) => applyStreamDelta(prev, e.messageId, e.text));
      } else {
        // completed/error: retire only this message's live stream; others may
        // still be in flight (parallel workflow stages).
        qc.invalidateQueries({ queryKey: ["chat", sessionRef.current] });
        qc.invalidateQueries({ queryKey: ["chats"] });
        setStreams((prev) => retireStream(prev, e.messageId));
        setOptimisticUser(null);
        setSending(false);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc]);

  useEffect(() => {
    setStreams(new Map());
    setOptimisticUser(null);
    setSending(false);
    setInput("");
    setMention(null);
    setSlash(null);
    setAttachments([]);
    if (stopTypingTimer.current) clearTimeout(stopTypingTimer.current);
    lastTypingPing.current = 0;
  }, [sessionId]);

  function scrollToBottom() {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight });
    setHasNew(false);
  }
  function onScroll() {
    const el = scrollRef.current;
    if (!el) return;
    const near = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    setAtBottom(near);
    if (near) setHasNew(false);
  }
  // Follow the conversation only when pinned to the bottom; if the user has
  // scrolled up, flag new content with a pill instead of yanking them down.
  useEffect(() => {
    if (atBottom) scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
    else setHasNew(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chat.data, streams, optimisticUser]);

  // A fresh chat always starts at the bottom.
  useEffect(() => {
    setAtBottom(true);
    setHasNew(false);
  }, [sessionId]);

  const currentRuntime = useMemo(
    () => runtimes.find((rt) => rt.id === currentRuntimeId) ?? runtimes[0] ?? null,
    [currentRuntimeId, runtimes],
  );

  function autoGrow() {
    const ta = taRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(Math.max(ta.scrollHeight, 56), 220)}px`;
  }

  // --- attachments ------------------------------------------------------
  function readAsBase64(file: File): Promise<string> {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        const res = String(reader.result);
        resolve(res.includes(",") ? res.slice(res.indexOf(",") + 1) : res);
      };
      reader.onerror = () => reject(reader.error ?? new Error("read failed"));
      reader.readAsDataURL(file);
    });
  }
  async function addFiles(files: FileList | File[]) {
    for (const file of Array.from(files)) {
      try {
        const b64 = await readAsBase64(file);
        const path = await saveAttachment(file.name || "file", b64);
        setAttachments((a) => [...a, { name: file.name || fileBaseName(path), path, image: isImagePath(path) }]);
      } catch (e) {
        toast.error(`Couldn't attach ${file.name}: ${errMsg(e)}`);
      }
    }
  }

  // --- @-mention autocomplete -------------------------------------------
  const memberList = useQuery({
    queryKey: ["members", sessionId],
    queryFn: () => listMembers(sessionId),
    enabled: Boolean(sessionId),
  });
  const agentList = useQuery({
    queryKey: ["agents", sessionId],
    queryFn: () => listAgents(sessionId),
    enabled: Boolean(sessionId),
  });
  const mentionCands = useMemo(() => {
    const out: { handle: string; kind: string }[] = [
      { handle: "primary", kind: "Primary runtime" },
      { handle: "all", kind: "Everyone" },
    ];
    for (const a of agentList.data ?? []) out.push({ handle: a.name, kind: "Agent" });
    for (const m of memberList.data ?? []) out.push({ handle: m.displayName, kind: m.role });
    return out.filter((c) => c.handle.trim().length > 0);
  }, [agentList.data, memberList.data]);
  const mentionMatches = useMemo(
    () => (mention ? filterMentions(mentionCands, mention.query) : []),
    [mention, mentionCands],
  );

  function acceptMention(handle: string) {
    const ta = taRef.current;
    if (!ta || !mention) return;
    const caret = ta.selectionStart ?? input.length;
    const next = `${input.slice(0, mention.start)}@${handle} ${input.slice(caret)}`;
    setInput(next);
    setMention(null);
    const pos = mention.start + handle.length + 2;
    requestAnimationFrame(() => {
      ta.focus();
      ta.setSelectionRange(pos, pos);
    });
  }


  // @file: pull a workspace file's contents into the composer as a fenced block,
  // so it rides into the model's context with the message.
  async function addFileRef() {
    const path = await promptDialog("Reference a workspace file", {
      placeholder: "path relative to the workspace root",
    });
    if (!path || !path.trim()) return;
    try {
      const content = await readWorkspaceFile(path.trim());
      const block = "```" + path.trim() + "\n" + content.replace(/\s+$/, "") + "\n```\n";
      setInput((cur) => (cur ? `${cur}\n${block}` : block));
      requestAnimationFrame(() => {
        taRef.current?.focus();
        autoGrow();
      });
    } catch (e) {
      toast.error(`Couldn't reference file: ${errMsg(e)}`);
    }
  }

  // Stable so the memoized last-assistant Bubble doesn't re-render every render
  // (e.g. on each composer keystroke).
  const handleRegenerate = useCallback(async () => {
    try {
      await regenerate(sessionId);
    } catch (e) {
      toast.error(`Couldn't regenerate: ${errMsg(e)}`);
    } finally {
      qc.invalidateQueries({ queryKey: ["chat", sessionId] });
    }
  }, [sessionId, qc]);

  function insertText(text: string) {
    setInput(text);
    setSlash(null);
    requestAnimationFrame(() => {
      const ta = taRef.current;
      if (ta) {
        ta.focus();
        ta.setSelectionRange(text.length, text.length);
        autoGrow();
      }
    });
  }
  async function exportChatToFile() {
    setSlash(null);
    setInput("");
    try {
      const path = await exportChat(sessionId);
      if (path) toast.success(`Exported transcript to ${path}`);
    } catch (e) {
      toast.error(`Export failed: ${errMsg(e)}`);
    }
  }
  async function runSummarize() {
    setSlash(null);
    setInput("");
    try {
      await summarizeChat(sessionId);
      qc.invalidateQueries({ queryKey: ["chat", sessionId] });
      toast.success("Summary added.");
    } catch (e) {
      toast.error(`Couldn't summarize: ${errMsg(e)}`);
    }
  }
  function runCompact() {
    setSlash(null);
    setInput("");
    confirmThen(
      "Compact this chat? Earlier messages are collapsed into a single summary and removed from the transcript.",
      async () => {
        try {
          await compactChat(sessionId);
          qc.invalidateQueries({ queryKey: ["chat", sessionId] });
          qc.invalidateQueries({ queryKey: ["chats"] });
          toast.success("Conversation compacted.");
        } catch (e) {
          toast.error(`Couldn't compact: ${errMsg(e)}`);
        }
      },
    );
  }
  async function pullLinearIssues() {
    setSlash(null);
    setInput("");
    try {
      const block = await linearIssuesContext();
      setInput((cur) => (cur ? `${cur}\n${block}\n` : `${block}\n`));
      requestAnimationFrame(() => {
        taRef.current?.focus();
        autoGrow();
      });
      toast.success("Linear issues added to the composer.");
    } catch (e) {
      toast.error(`Couldn't fetch Linear issues: ${errMsg(e)}`);
    }
  }
  const slashItems = useMemo(() => {
    if (!slash) return [];
    const all: { id: string; label: string; hint: string; run: () => void }[] = [
      { id: "summarize", label: "Summarize conversation", hint: "Context", run: () => void runSummarize() },
      { id: "compact", label: "Compact conversation", hint: "Context", run: () => runCompact() },
      { id: "linear", label: "Insert Linear issues", hint: "Context", run: () => void pullLinearIssues() },
      { id: "export", label: "Export chat → Markdown", hint: "File", run: () => void exportChatToFile() },
      { id: "clear", label: "Clear input", hint: "Edit", run: () => setInput("") },
      ...loadTemplates().map((t) => ({
        id: t.id,
        label: t.name,
        hint: "Template",
        run: () => insertText(t.body),
      })),
    ];
    const q = slash.query.toLowerCase();
    return all.filter((i) => i.label.toLowerCase().includes(q)).slice(0, 8);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [slash]);

  async function handleSend() {
    const text = input.trim();
    if ((!text && attachments.length === 0) || sending) return;
    // Append attachment path markers; agents read the path, the API runtime
    // inlines image markers as vision blocks.
    const markers = attachments.map((a) => attachmentMarker(a.path)).join("\n");
    const body = [text, markers].filter(Boolean).join("\n\n");
    const sentAttachments = attachments;
    setInput("");
    setAttachments([]);
    setMention(null);
    setSlash(null);
    clearTyping();
    setOptimisticUser(body);
    setSending(true);
    if (taRef.current) taRef.current.style.height = "auto";
    try {
      await sendMessage(sessionId, body);
    } catch (e) {
      // Keep the draft + attachments so nothing is lost, and offer a retry.
      setSending(false);
      setOptimisticUser(null);
      setInput((cur) => cur || text);
      setAttachments((cur) => (cur.length ? cur : sentAttachments));
      toast.error(`Couldn't send: ${errMsg(e)}`, { label: "Retry", run: () => void handleSend() });
    } finally {
      qc.invalidateQueries({ queryKey: ["chat", sessionId] });
      qc.invalidateQueries({ queryKey: ["chats"] });
    }
  }

  const messages: ChatMessageDto[] = chat.data?.messages ?? [];
  const lastAssistantId = [...messages]
    .reverse()
    .find((m) => m.role === "assistant" || m.role === "agent")?.id;

  return (
    <div className={`flex h-full min-w-0 flex-col ${embedded ? "" : "p-4"}`}>
      <div
        className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-3xl border"
        style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
      >
        <div className="relative flex min-h-0 flex-1 flex-col">
        <div ref={scrollRef} onScroll={onScroll} className="flex-1 space-y-4 overflow-y-auto px-4 py-5">
          {chat.isLoading && messages.length === 0 && <SkeletonBubbles count={3} />}
          {!chat.isLoading && messages.length === 0 && !optimisticUser && (
            <div
              className="max-w-lg rounded-3xl border p-6"
              style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
            >
              <div
                className="flex h-11 w-11 items-center justify-center rounded-2xl"
                style={{ background: "rgba(87,161,168,0.18)", color: "var(--hive-accent-cool)" }}
                aria-hidden
              >
                <IconMessage size={22} />
              </div>
              <div className="mt-4 text-2xl font-semibold tracking-tight">Start a conversation</div>
              <p className="mt-2 text-base opacity-65">
                Ask about your code, or @mention an agent to have it make changes. Try one:
              </p>
              <div className="mt-4 flex flex-col gap-2">
                {STARTER_PROMPTS.map((p) => (
                  <button
                    key={p}
                    onClick={() => {
                      setInput(p);
                      requestAnimationFrame(() => {
                        taRef.current?.focus();
                        autoGrow();
                      });
                    }}
                    className="group/starter flex items-center gap-2 rounded-xl border px-3.5 py-2.5 text-left text-sm transition-all hover:border-transparent hover:shadow-sm"
                    style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
                  >
                    <span className="flex-1">{p}</span>
                    <span
                      className="shrink-0 -rotate-90 opacity-0 transition-opacity group-hover/starter:opacity-60"
                      style={{ color: "var(--hive-accent-cool)" }}
                      aria-hidden
                    >
                      <IconArrowDown size={14} />
                    </span>
                  </button>
                ))}
              </div>
              <div
                className="mt-5 flex flex-wrap items-center gap-x-4 gap-y-1 border-t pt-4 text-xs opacity-55"
                style={{ borderColor: "var(--hive-line)" }}
              >
                <span><code>/</code> for commands</span>
                <span><code>@file</code> to reference a file</span>
                <button onClick={onOpenTools} className="underline underline-offset-2 hover:opacity-100">
                  Configure agents &amp; tools
                </button>
              </div>
            </div>
          )}
          {messages
            // A message that's mid-stream is rendered by the live `streams`
            // bubble below; skip its (partially-flushed) transcript copy so it
            // doesn't render twice — happens whenever the chat query refetches
            // mid-stream (reactions, parallel workflow stages).
            .filter((m) => !(m.isStreaming && streams.has(m.id)))
            .map((m) => (
            <Bubble
              key={m.id}
              messageId={m.id}
              role={m.role}
              author={m.author}
              body={m.body}
              createdAt={m.createdAt}
              streaming={m.isStreaming}
              reactions={m.reactions}
              toolCalls={m.toolCalls}
              toolResults={m.toolResults}
              onReact={handleReact}
              onRegenerate={m.id === lastAssistantId && !sending && streams.size === 0 ? handleRegenerate : undefined}
            />
          ))}
          {optimisticUser && <Bubble role="user" author={selfName} body={optimisticUser} />}
          {[...streams.entries()].map(([id, text]) => (
            <Bubble key={id} role="assistant" author="Hive" body={text} streaming />
          ))}
          {sending && streams.size === 0 && <TypingDots label="Hive is thinking" />}
          {typingNames.length > 0 && <TypingDots label={typingLabel(typingNames)} />}
        </div>
          {hasNew && !atBottom && (
            <button
              onClick={scrollToBottom}
              className="absolute bottom-4 left-1/2 flex -translate-x-1/2 items-center gap-1.5 rounded-full px-3.5 py-1.5 text-xs font-semibold shadow-lg transition-transform hover:scale-105"
              style={{ background: "var(--hive-accent-cool)", color: "#fff" }}
            >
              <span aria-hidden><IconArrowDown size={13} /></span>
              New messages
            </button>
          )}
        </div>

        {currentRuntime?.provider === "claude-code" && !claudeNoteDismissed && (
          <div
            className="mx-4 mt-3 flex items-start gap-2.5 rounded-xl border px-3.5 py-2.5 text-xs leading-5"
            style={{
              borderColor: "var(--hive-line)",
              background: "rgba(87,161,168,0.10)",
            }}
          >
            <span aria-hidden className="mt-px shrink-0 opacity-70" style={{ color: "var(--hive-accent-cool)" }}>
              <IconInfo size={14} />
            </span>
            <span className="flex-1 opacity-75">
              Claude Code is the agent — it handles tools and permissions on this machine using your
              Claude subscription.
            </span>
            <button
              onClick={() => {
                window.localStorage.setItem(CLAUDE_NOTE_KEY, "1");
                setClaudeNoteDismissed(true);
              }}
              className="shrink-0 rounded-md px-1.5 py-0.5 leading-none opacity-50 transition-opacity hover:opacity-100"
              title="Dismiss"
              aria-label="Dismiss"
            >
              <IconX size={12} />
            </button>
          </div>
        )}

        <div className="border-t px-4 py-3.5" style={{ borderColor: "var(--hive-line)" }}>
          <div className="mb-2.5 flex flex-wrap items-center gap-2">
            <label className="inline-flex min-w-[16rem] items-center gap-2.5 rounded-xl border px-2.5 py-1.5 transition-colors focus-within:border-[color:var(--hive-accent-cool)]" style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}>
              <span className="shrink-0 text-[10px] font-medium uppercase tracking-[0.12em] opacity-45">
                Primary runtime
              </span>
              <select
                value={currentRuntimeId}
                onChange={async (e) => {
                  await setChatRuntime(sessionId, e.target.value);
                  qc.invalidateQueries({ queryKey: ["chat", sessionId] });
                  qc.invalidateQueries({ queryKey: ["chats"] });
                }}
                className="min-w-0 flex-1 appearance-none bg-transparent pr-1 text-sm font-medium outline-none"
                style={{
                  color: "var(--hive-ink)",
                  fontFamily: "inherit",
                }}
              >
                {runtimes.map((rt) => (
                  <option
                    key={rt.id}
                    value={rt.id}
                    title={`${rt.label} (${rt.provider || "provider unknown"}${rt.model ? ` · ${rt.model}` : ""})`}
                  >
                    {runtimePickerLabel(rt)}
                  </option>
                ))}
              </select>
              <span className="opacity-40"><IconChevronDown size={12} /></span>
            </label>
          </div>

          {attachments.length > 0 && (
            <div className="mb-2 flex flex-wrap gap-2">
              {attachments.map((a, i) => (
                <span
                  key={`${a.path}-${i}`}
                  className="inline-flex items-center gap-1.5 rounded-lg border px-2 py-1 text-xs"
                  style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
                  title={a.path}
                >
                  <span aria-hidden className="opacity-60">
                    {a.image ? <IconImage size={13} /> : <IconFile size={13} />}
                  </span>
                  <span className="max-w-[12rem] truncate">{a.name}</span>
                  <button
                    className="rounded opacity-50 transition-opacity hover:opacity-100"
                    aria-label={`Remove attachment ${a.name}`}
                    onClick={() => setAttachments((cur) => cur.filter((_, j) => j !== i))}
                  >
                    <IconX size={12} />
                  </button>
                </span>
              ))}
            </div>
          )}

          <div
            className="relative flex min-w-0 flex-col rounded-2xl border transition-colors focus-within:border-[color:var(--hive-accent-cool)]"
            style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
            onDrop={(e) => {
              if (e.dataTransfer.files.length) {
                e.preventDefault();
                void addFiles(e.dataTransfer.files);
              }
            }}
            onDragOver={(e) => {
              if (e.dataTransfer.types.includes("Files")) e.preventDefault();
            }}
          >
            {slash && slashItems.length > 0 && (
              <div
                className="absolute bottom-full left-0 z-20 mb-2 w-80 overflow-hidden rounded-xl border p-1 shadow-xl"
                style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
              >
                {slashItems.map((c, i) => (
                  <button
                    key={c.id}
                    onMouseEnter={() => setSlashActive(i)}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      c.run();
                    }}
                    className="flex w-full items-center justify-between gap-3 rounded-lg px-2.5 py-2 text-left text-sm transition-colors"
                    style={{ background: i === slashActive ? "var(--hive-mist)" : "transparent" }}
                  >
                    <span className="truncate">{c.label}</span>
                    <span className="shrink-0 text-xs opacity-45">{c.hint}</span>
                  </button>
                ))}
              </div>
            )}
            {mention && mentionMatches.length > 0 && (
              <div
                className="absolute bottom-full left-0 z-20 mb-2 w-72 overflow-hidden rounded-xl border p-1 shadow-xl"
                style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
              >
                {mentionMatches.map((c, i) => (
                  <button
                    key={c.handle}
                    onMouseEnter={() => setMentionActive(i)}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      acceptMention(c.handle);
                    }}
                    className="flex w-full items-center justify-between gap-3 rounded-lg px-2.5 py-2 text-left text-sm transition-colors"
                    style={{ background: i === mentionActive ? "var(--hive-mist)" : "transparent" }}
                  >
                    <span className="truncate font-medium">@{c.handle}</span>
                    <span className="shrink-0 text-xs capitalize opacity-45">{c.kind}</span>
                  </button>
                ))}
              </div>
            )}
            <textarea
              ref={taRef}
              value={input}
              onPaste={(e) => {
                const files = Array.from(e.clipboardData.files);
                if (files.length) {
                  e.preventDefault();
                  void addFiles(files);
                }
              }}
              onChange={(e) => {
                const v = e.target.value;
                const caret = e.target.selectionStart ?? v.length;
                setInput(v);
                autoGrow();
                const sl = detectSlash(v, caret);
                setSlash(sl);
                setSlashActive(0);
                setMention(sl ? null : detectMention(v, caret));
                setMentionActive(0);
                if (v.trim()) pingTyping();
                else clearTyping();
              }}
              onBlur={clearTyping}
              onKeyDown={(e) => {
                if (slash && slashItems.length > 0) {
                  if (e.key === "ArrowDown") {
                    e.preventDefault();
                    setSlashActive((a) => Math.min(a + 1, slashItems.length - 1));
                    return;
                  }
                  if (e.key === "ArrowUp") {
                    e.preventDefault();
                    setSlashActive((a) => Math.max(a - 1, 0));
                    return;
                  }
                  if (e.key === "Enter" || e.key === "Tab") {
                    e.preventDefault();
                    slashItems[slashActive].run();
                    return;
                  }
                  if (e.key === "Escape") {
                    e.preventDefault();
                    setSlash(null);
                    return;
                  }
                }
                if (mention && mentionMatches.length > 0) {
                  if (e.key === "ArrowDown") {
                    e.preventDefault();
                    setMentionActive((a) => Math.min(a + 1, mentionMatches.length - 1));
                    return;
                  }
                  if (e.key === "ArrowUp") {
                    e.preventDefault();
                    setMentionActive((a) => Math.max(a - 1, 0));
                    return;
                  }
                  if (e.key === "Enter" || e.key === "Tab") {
                    e.preventDefault();
                    acceptMention(mentionMatches[mentionActive].handle);
                    return;
                  }
                  if (e.key === "Escape") {
                    e.preventDefault();
                    setMention(null);
                    return;
                  }
                }
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  handleSend();
                }
              }}
              rows={1}
              placeholder="Message Hive or describe the next supervised task…"
              className="w-full resize-none bg-transparent px-4 pt-3 pb-1 text-sm leading-6 outline-none"
              style={{
                color: "var(--hive-ink)",
                minHeight: 48,
                fontFamily: "inherit",
              }}
            />
            <div className="flex items-center gap-1 px-2 pb-2">
              <label
                className="flex h-8 w-8 cursor-pointer items-center justify-center rounded-lg opacity-60 transition-all hover:bg-[color:var(--hive-panel)] hover:opacity-100"
                title="Attach files"
                aria-label="Attach files"
              >
                <IconPaperclip />
                <input
                  type="file"
                  multiple
                  className="hidden"
                  onChange={(e) => {
                    if (e.target.files?.length) void addFiles(e.target.files);
                    e.target.value = "";
                  }}
                />
              </label>
              <button
                type="button"
                onClick={() => void addFileRef()}
                className="flex h-8 items-center justify-center rounded-lg px-2 text-xs opacity-60 transition-all hover:bg-[color:var(--hive-panel)] hover:opacity-100"
                title="Reference a workspace file (@file)"
              >
                @file
              </button>
              <div className="flex-1" />
              <button
                onClick={handleSend}
                disabled={sending || (!input.trim() && attachments.length === 0)}
                className="flex h-8 w-8 items-center justify-center rounded-full text-white shadow-sm transition-all hover:brightness-105 disabled:cursor-not-allowed disabled:opacity-30 disabled:shadow-none disabled:hover:brightness-100"
                style={{ background: "var(--hive-accent-cool)" }}
                aria-label="Send message"
                title="Send (Enter)"
              >
                <IconSend />
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function typingLabel(names: string[]): string {
  if (names.length === 1) return `${names[0]} is typing`;
  if (names.length === 2) return `${names[0]} and ${names[1]} are typing`;
  return "Several people are typing";
}

/// Animated "… is typing"/"is thinking" row, shown as a lightweight pending
/// bubble so it reads as an in-flight turn rather than floating text.
function TypingDots({ label }: { label: string }) {
  return (
    <div
      className="inline-flex items-center gap-2 rounded-2xl border px-3.5 py-2.5 text-sm"
      style={{ borderColor: "rgba(87,161,168,0.26)", background: "rgba(87,161,168,0.10)" }}
    >
      <span className="opacity-70">{label}</span>
      <span className="inline-flex gap-0.5">
        <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-current opacity-60 [animation-delay:-0.2s]" />
        <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-current opacity-60 [animation-delay:-0.1s]" />
        <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-current opacity-60" />
      </span>
    </div>
  );
}

/// Renders a message: markdown text plus chips for any `[Attached: …]` paths.
function MessageBody({ body }: { body: string }) {
  const { text, paths } = splitAttachments(body);
  return (
    <>
      {text && <Markdown content={text} />}
      {paths.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-2">
          {paths.map((p, i) => (
            <span
              key={`${p}-${i}`}
              className="inline-flex items-center gap-1.5 rounded-lg border px-2 py-1 text-xs"
              style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
              title={p}
            >
              <span aria-hidden className="opacity-60">
                {isImagePath(p) ? <IconImage size={13} /> : <IconFile size={13} />}
              </span>
              <span className="max-w-[14rem] truncate">{fileBaseName(p)}</span>
            </span>
          ))}
        </div>
      )}
    </>
  );
}

function prettyJson(s: string): string {
  try {
    return JSON.stringify(JSON.parse(s), null, 2);
  } catch {
    return s;
  }
}

/// Inline cards for the tool calls an assistant turn made, each collapsible to
/// its arguments + (matched-by-id) result. Exported for unit testing.
export function ToolCallCards({
  calls,
  results,
}: {
  calls: { id: string; name: string; inputJson: string; serverId: string | null }[];
  results: { callId: string; content: string; isError: boolean }[];
}) {
  return (
    <div className="mt-3 space-y-2">
      {calls.map((c) => {
        const result = results.find((r) => r.callId === c.id);
        return (
          <details
            key={c.id}
            className="overflow-hidden rounded-xl border text-sm transition-colors"
            style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
          >
            <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 transition-colors hover:bg-[color:var(--hive-overlay)]">
              <span aria-hidden className="opacity-70"><IconWrench size={13} /></span>
              <span className="truncate font-mono font-medium">{c.name}</span>
              {c.serverId && <span className="shrink-0 text-xs opacity-50">· {c.serverId}</span>}
              {result && (
                <span
                  className="ml-auto shrink-0 rounded-full px-2 py-0.5 text-[0.7rem] font-medium"
                  style={
                    result.isError
                      ? { background: "rgba(214,90,70,0.16)", color: "var(--hive-danger)" }
                      : { background: "rgba(34,160,90,0.14)", color: "var(--hive-success)" }
                  }
                >
                  {result.isError ? "error" : "done"}
                </span>
              )}
            </summary>
            <div className="space-y-2 px-3 pb-3">
              {c.inputJson && (
                <pre
                  className="overflow-x-auto rounded-lg p-2 text-xs"
                  style={{ background: "var(--hive-overlay)", border: "1px solid var(--hive-line)" }}
                >
                  {prettyJson(c.inputJson)}
                </pre>
              )}
              {result && (
                <pre
                  className="overflow-x-auto rounded-lg p-2 text-xs"
                  style={{
                    background: result.isError ? "rgba(214,90,70,0.14)" : "var(--hive-overlay)",
                    border: "1px solid var(--hive-line)",
                  }}
                >
                  {result.content.length > 4000 ? `${result.content.slice(0, 4000)}…` : result.content}
                </pre>
              )}
            </div>
          </details>
        );
      })}
    </div>
  );
}

const Bubble = memo(function Bubble({
  messageId,
  role,
  author,
  body,
  createdAt,
  streaming = false,
  reactions,
  toolCalls,
  toolResults,
  onReact,
  onRegenerate,
}: {
  messageId?: string;
  role: string;
  author: string;
  body: string;
  createdAt?: string;
  streaming?: boolean;
  reactions?: { emoji: string; actorId: string; actorDisplayName: string }[];
  toolCalls?: { id: string; name: string; inputJson: string; serverId: string | null }[];
  toolResults?: { callId: string; content: string; isError: boolean }[];
  onReact?: (messageId: string, emoji: string) => void;
  onRegenerate?: () => void;
}) {
  const time = createdAt ? new Date(createdAt) : null;
  const timeLabel =
    time && !Number.isNaN(time.getTime())
      ? time.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
      : "";
  const isUser = role === "user";
  const isSystem = role === "system";
  const counts = new Map<string, number>();
  for (const r of reactions ?? []) counts.set(r.emoji, (counts.get(r.emoji) ?? 0) + 1);
  const [reactOpen, setReactOpen] = useState(false);

  // Modern chat: user messages hug the right in a warm bubble, assistant/agent
  // hug the left in a cool bubble. A hairline border of the same hue keeps each
  // bubble defined against the panel in every palette.
  const surface = isUser
    ? { bg: "rgba(214,158,87,0.16)", border: "rgba(214,158,87,0.30)", avatar: "rgba(214,158,87,0.32)" }
    : { bg: "rgba(87,161,168,0.12)", border: "rgba(87,161,168,0.22)", avatar: "rgba(87,161,168,0.30)" };

  const content = streaming ? (
    <div className="whitespace-pre-wrap text-[0.95rem] leading-7">
      {body}
      <span className="ml-0.5 inline-block h-[1.1em] w-[2px] translate-y-[2px] animate-pulse bg-current align-middle" />
    </div>
  ) : body ? (
    <MessageBody body={body} />
  ) : toolCalls && toolCalls.length > 0 ? null : (
    <div className="text-[0.95rem] leading-7 opacity-50">…</div>
  );

  // System messages (summaries, notices) read as centered meta, not a reply.
  if (isSystem) {
    return (
      <div
        className="mx-auto max-w-[85%] rounded-xl border px-3.5 py-2 text-center text-xs"
        style={{ background: "var(--hive-mist)", borderColor: "var(--hive-line)", color: "var(--hive-ink)" }}
      >
        <span className="mr-2 uppercase tracking-[0.16em] opacity-45">System</span>
        <span className="opacity-80">{body}</span>
      </div>
    );
  }

  return (
    // `content-visibility: auto` lets the engine skip layout/paint for rows
    // scrolled out of view — cheap "virtualization" for long transcripts, most
    // relevant on WebView2/Windows. Skipped for the live streaming bubble.
    <div
      className={`group flex gap-2.5 ${isUser ? "flex-row-reverse" : "flex-row"}`}
      style={streaming ? undefined : { contentVisibility: "auto", containIntrinsicSize: "0 120px" }}
    >
      <div
        className="mt-6 flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-xs font-semibold"
        style={{ background: surface.avatar, color: "var(--hive-ink)" }}
        aria-hidden
      >
        {author.slice(0, 1).toUpperCase()}
      </div>

      <div className={`flex min-w-0 max-w-[82%] flex-col ${isUser ? "items-end" : "items-start"}`}>
        <div className="mb-1 flex items-center gap-2 px-1 text-xs">
          <span className="font-semibold">{author}</span>
          {timeLabel && <span className="opacity-40">{timeLabel}</span>}
        </div>

        <div
          className={`min-w-0 border px-4 py-2.5 ${isUser ? "rounded-2xl rounded-tr-sm" : "rounded-2xl rounded-tl-sm"}`}
          style={{ background: surface.bg, borderColor: surface.border, color: "var(--hive-ink)" }}
        >
          {content}
          {toolCalls && toolCalls.length > 0 && (
            <ToolCallCards calls={toolCalls} results={toolResults ?? []} />
          )}
        </div>

        {/* Persisted reactions stay visible; the action row (copy / regenerate /
            react-picker) only appears on hover, so no emoji strip floats under
            every message. */}
        <div className={`mt-1 flex min-h-[1.5rem] items-center gap-1 ${isUser ? "flex-row-reverse" : ""}`}>
          {[...counts.entries()].map(([emoji, n]) => (
            <button
              key={emoji}
              onClick={() => onReact?.(messageId!, emoji)}
              className="rounded-full border px-2 py-0.5 text-xs transition-colors hover:border-[color:var(--hive-accent-cool)]"
              style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
            >
              {emoji} {n}
            </button>
          ))}
          <div className="flex items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
            {!streaming && body && <CopyAction body={body} />}
            {!streaming && onRegenerate && (
              <IconAction title="Regenerate" onClick={() => void onRegenerate()}>
                <IconRegenerate />
              </IconAction>
            )}
            {onReact && messageId && (
              <div className="relative">
                <IconAction title="Add reaction" onClick={() => setReactOpen((o) => !o)}>
                  <IconSmile />
                </IconAction>
                {reactOpen && (
                  <div
                    className="absolute bottom-full z-20 mb-1 flex gap-0.5 rounded-lg border p-1 shadow-lg"
                    style={{
                      borderColor: "var(--hive-line)",
                      background: "var(--hive-panel)",
                      ...(isUser ? { right: 0 } : { left: 0 }),
                    }}
                    onMouseLeave={() => setReactOpen(false)}
                  >
                    {QUICK_EMOJI.map((emoji) => (
                      <button
                        key={emoji}
                        onClick={() => {
                          onReact(messageId, emoji);
                          setReactOpen(false);
                        }}
                        className="rounded px-1.5 py-0.5 text-sm transition-transform hover:scale-125"
                        aria-label={`React ${emoji}`}
                      >
                        {emoji}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
});

/// Copy-message action with transient "copied" feedback (icon flips to a
/// check) — a silent clipboard write left users unsure it worked.
function CopyAction({ body }: { body: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <span style={copied ? { color: "var(--hive-success)" } : undefined}>
      <IconAction
        title={copied ? "Copied" : "Copy"}
        onClick={() => {
          void navigator.clipboard.writeText(body);
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1500);
        }}
      >
        {copied ? <IconCheck /> : <IconCopy />}
      </IconAction>
    </span>
  );
}

/// Compact ghost icon-button for the hover action row under a message.
function IconAction({
  title,
  onClick,
  children,
}: {
  title: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      aria-label={title}
      className="flex h-6 w-6 items-center justify-center rounded-md opacity-60 transition-all hover:opacity-100"
      onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hive-mist)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      {children}
    </button>
  );
}
