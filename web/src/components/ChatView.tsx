import { Fragment, memo, useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
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
  readImageDataUrl,
  readWorkspaceFile,
  sendMessage,
  stopTurn,
  regenerate,
  summarizeChat,
  compactChat,
  linearIssuesContext,
  setChatRuntime,
  syncStatus,
  toggleReaction,
  type ChatMessageDto,
  type RuntimeSummaryDto,
  type WorkspaceAgentDto,
} from "@/lib/ipc";
import { attachmentMarker, splitAttachments, fileBaseName, isImagePath } from "@/lib/attachments";
import { relTime, absTime, clockTime, dayKey, dayLabel } from "@/lib/time";
import { PRIMARY_NAME, PRIMARY_HANDLE } from "@/lib/agent";
import {
  IconCopy,
  IconRegenerate,
  IconSmile,
  IconSend,
  IconStop,
  IconPaperclip,
  IconFile,
  IconImage,
  IconInfo,
  IconWrench,
  IconArrowDown,
  IconMessage,
  IconX,
  IconChevronDown,
  IconChevronRight,
  IconCheck,
  IconUsers,
} from "@/lib/icons";
import { toast, errMsg } from "@/components/Toast";
import { SkeletonBubbles } from "@/components/Skeleton";
import { EmojiPicker } from "@/components/EmojiPicker";
import {
  loadEmojiIndex,
  searchEmoji,
  exactEmoji,
  detectShortcode,
  closingShortcodeWord,
  type EmojiEntry,
} from "@/lib/emoji";
import { Avatar } from "@/components/Avatar";
import { Popover, PopoverItem, candidateKey, ErrorState } from "@/components/ui";
import { Markdown } from "@/components/Markdown";
import { detectMention, filterMentions } from "@/lib/mentions";
import { applyStreamDelta, retireStream } from "@/lib/streams";
import { pinToBottom, pinAfterScroll } from "@/lib/autoscroll";
import { detectSlash } from "@/lib/slash";
import { confirmThen } from "@/lib/confirm";
import { promptDialog } from "@/components/Dialog";
import { loadTemplates } from "@/lib/templates";

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
  const [sending, setSending] = useState(false);
  const [optimisticUser, setOptimisticUser] = useState<string | null>(null);
  // messageId → accumulated text. A Map (not a single slot) because workflow
  // fan-out streams several assistant messages into the chat concurrently.
  const [streams, setStreams] = useState<Map<string, string>>(new Map());
  // Set when a turn dies mid-stream (phase "error") so a failed generation is
  // visibly distinct from a finished one; cleared on the next send / session.
  const [streamError, setStreamError] = useState<string | null>(null);
  // Watchdog: a turn is "stalled" when it's been in flight with no delta for a
  // while. The backend can resolve `sendMessage` yet emit nothing (a hung CLI, a
  // dropped event), leaving "thinking" forever with no recovery — this surfaces a
  // Stop-and-retry affordance instead of an indefinite spinner. `lastActivityRef`
  // is the ms timestamp of the last send/delta (a ref so deltas don't re-render).
  const [stalled, setStalled] = useState(false);
  const lastActivityRef = useRef(0);
  // Message ids whose terminal (completed/error) we've already handled, so a
  // duplicate terminal for the same message can't re-run retire (P2-11).
  const retiredIdsRef = useRef<Set<string>>(new Set());
  // Autoscroll only when already pinned to the bottom; otherwise surface a pill.
  const [atBottom, setAtBottom] = useState(true);
  // Mirrors `atBottom` for the pin loop, which runs across frames and needs the
  // live value, not the one captured when it started.
  const atBottomRef = useRef(true);
  // Last position the scroll handler saw, so it can tell a reader scrolling up
  // from the layout growing underneath a settling pin (lib/autoscroll).
  const scrollMark = useRef(0);
  const [hasNew, setHasNew] = useState(false);
  // @-mention autocomplete in the composer.
  const [mention, setMention] = useState<{ start: number; query: string } | null>(null);
  const [mentionActive, setMentionActive] = useState(0);
  // /-slash command menu in the composer.
  const [slash, setSlash] = useState<{ query: string } | null>(null);
  const [slashActive, setSlashActive] = useState(0);
  // Pending composer attachments (saved to disk; referenced by path on send).
  const [attachments, setAttachments] = useState<{ name: string; path: string; image: boolean }[]>([]);
  // Composer emoji: the picker-panel toggle + the inline `:shortcode` menu.
  const [showEmoji, setShowEmoji] = useState(false);
  const [emojiIndex, setEmojiIndex] = useState<EmojiEntry[]>([]);
  const [emojiSc, setEmojiSc] = useState<{ start: number; query: string } | null>(null);
  const [emojiScActive, setEmojiScActive] = useState(0);
  const emojiPanelRef = useRef<HTMLDivElement>(null);
  // Composer route pill: the "default recipient when no @mention" popover.
  const [routeOpen, setRouteOpen] = useState(false);
  // Composer focus ring (the turn-shaped card lights its accent border).
  const [composerFocused, setComposerFocused] = useState(false);
  const routeRef = useRef<HTMLDivElement>(null);
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
  const selfAvatarUrl = appSettings.data?.avatarUrl ?? undefined;

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
        // Progress → reset the stall watchdog (setState is a no-op when already
        // false, so this is cheap even at token rate).
        lastActivityRef.current = Date.now();
        setStalled(false);
        setStreams((prev) => applyStreamDelta(prev, e.messageId, e.text));
      } else {
        // completed/error: retire only this message's live stream; others may
        // still be in flight (parallel workflow stages). An "error" phase means
        // the turn died mid-generation — flag it distinctly (a died-mid-stream
        // turn must not read as a finished one).
        if (e.phase === "error") {
          setStreamError(e.text?.trim() || "Generation failed or was interrupted.");
        }
        // Land the persisted turn in the transcript BEFORE retiring its live
        // stream bubble. If we retire first, the streamed text vanishes for a
        // frame (stream gone, refetch not back yet) and the bottom-anchor spacer
        // recomputes — so the whole thread visibly jumps up then drops. Refetch,
        // then retire, so the persisted copy replaces the bubble seamlessly.
        const mid = e.messageId;
        // Idempotency guard (P2-11): ignore a DUPLICATE terminal for a message we
        // already retired. Without this, a repeated completed/error for one message
        // re-runs retire when `streams` is empty and prematurely tears down a
        // *sibling* turn's "thinking"/optimistic state (fan-out / back-to-back
        // turns). The first terminal for a message still proceeds normally, so the
        // zero-delta completion path is unaffected. (Full out-of-order correlation
        // across distinct messages would need a backend per-turn id.)
        if (retiredIdsRef.current.has(mid)) return;
        retiredIdsRef.current.add(mid);
        const retire = () =>
          setStreams((prev) => {
            const next = retireStream(prev, mid);
            // Clear the shared "sending"/optimistic state only when the LAST live
            // stream retires — a sibling parallel workflow-stage stream may still
            // be generating, so clearing on the first completion read as "done"
            // while others were mid-flight.
            if (next.size === 0) {
              setSending(false);
              setOptimisticUser(null);
            }
            return next;
          });
        void qc
          .invalidateQueries({ queryKey: ["chat", sessionRef.current] })
          .finally(() => {
            void qc.invalidateQueries({ queryKey: ["chats"] });
            retire();
          });
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc]);

  useEffect(() => {
    setStreams(new Map());
    setStreamError(null);
    setStalled(false);
    retiredIdsRef.current = new Set();
    setOptimisticUser(null);
    setSending(false);
    setInput("");
    setMention(null);
    setSlash(null);
    setAttachments([]);
    // A fresh chat always starts at the bottom. Re-pinning here (rather than in
    // an effect declared after the follow effect below) matters on a session
    // switch: the follow effect fires on the incoming chat's data and must not
    // see the outgoing session's "reader had scrolled up" state.
    atBottomRef.current = true;
    setAtBottom(true);
    setHasNew(false);
    scrollMark.current = 0;
    if (stopTypingTimer.current) clearTimeout(stopTypingTimer.current);
    lastTypingPing.current = 0;
  }, [sessionId]);

  // Jump to the newest turn and hold there while the layout settles — one
  // scroll isn't enough, because the turn rows' `content-visibility` makes
  // scrollHeight an underestimate until they're really laid out (lib/autoscroll).
  const cancelPin = useRef<(() => void) | null>(null);
  const pin = useCallback(() => {
    cancelPin.current?.();
    cancelPin.current = pinToBottom(() => scrollRef.current, {
      pinned: () => atBottomRef.current,
    });
  }, []);
  useEffect(() => () => cancelPin.current?.(), []);

  function setPinned(next: boolean) {
    atBottomRef.current = next;
    setAtBottom(next);
  }
  function scrollToBottom() {
    setPinned(true);
    setHasNew(false);
    pin();
  }
  function onScroll() {
    const el = scrollRef.current;
    if (!el) return;
    const next = pinAfterScroll({ top: scrollMark.current, pinned: atBottomRef.current }, el);
    scrollMark.current = el.scrollTop;
    setPinned(next);
    if (next) setHasNew(false);
  }
  // Follow the conversation only when pinned to the bottom; if the user has
  // scrolled up, flag new content with a pill instead of yanking them down.
  // This also covers mount: switching to another view unmounts ChatView, so
  // coming back re-runs it and re-pins the freshly rendered transcript.
  useEffect(() => {
    if (atBottomRef.current) pin();
    else setHasNew(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chat.data, streams, optimisticUser]);

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

  // --- emoji (`:shortcode` autocomplete + picker) -----------------------
  // Load the emoji index once (lazy chunk); shared with the reaction picker.
  useEffect(() => {
    let alive = true;
    void loadEmojiIndex().then((idx) => {
      if (alive) setEmojiIndex(idx);
    });
    return () => {
      alive = false;
    };
  }, []);
  const emojiMatches = useMemo(
    () => (emojiSc ? searchEmoji(emojiIndex, emojiSc.query) : []),
    [emojiSc, emojiIndex],
  );
  // Close the picker panel on outside click / Escape.
  useEffect(() => {
    if (!showEmoji) return;
    const onDown = (ev: MouseEvent) => {
      if (!emojiPanelRef.current?.contains(ev.target as Node)) setShowEmoji(false);
    };
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") setShowEmoji(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [showEmoji]);
  // Insert an emoji at the caret (from the picker panel).
  function insertEmoji(emoji: string) {
    const ta = taRef.current;
    const start = ta?.selectionStart ?? input.length;
    const end = ta?.selectionEnd ?? input.length;
    const next = input.slice(0, start) + emoji + input.slice(end);
    setInput(next);
    requestAnimationFrame(() => {
      if (!ta) return;
      ta.focus();
      const pos = start + emoji.length;
      ta.setSelectionRange(pos, pos);
      autoGrow();
    });
  }
  // Replace the active `:shortcode` with the chosen emoji.
  function acceptEmojiShortcode(emoji: string) {
    if (!emojiSc) return;
    const sc = emojiSc;
    const end = sc.start + 1 + sc.query.length;
    setInput((cur) => cur.slice(0, sc.start) + emoji + cur.slice(end));
    setEmojiSc(null);
    requestAnimationFrame(() => {
      const ta = taRef.current;
      if (!ta) return;
      ta.focus();
      const pos = sc.start + emoji.length;
      ta.setSelectionRange(pos, pos);
      autoGrow();
    });
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
  // Author (handle) → roster agent, so a turn can tell a personal agent (owner
  // device) from a workspace-owned "shared" one (empty ownerActorId), and pick
  // up the agent's stored avatar tint as its turn accent.
  const agentByName = useMemo(() => {
    const m = new Map<string, WorkspaceAgentDto>();
    for (const a of agentList.data ?? []) m.set(a.name, a);
    return m;
  }, [agentList.data]);
  const mentionCands = useMemo(() => {
    const out: { handle: string; kind: string }[] = [
      { handle: PRIMARY_HANDLE, kind: "Default agent" },
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
    // Clear any prior failure banner up front — otherwise the stale red
    // "Generation failed" message keeps shouting over the new (often successful)
    // regenerated turn as it streams in.
    setStreamError(null);
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
    setStreamError(null);
    clearTyping();
    setOptimisticUser(body);
    setSending(true);
    lastActivityRef.current = Date.now();
    setStalled(false);
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

  // Stop the in-flight turn and free the composer immediately. The backend
  // aborts generation (killing any CLI subprocess); locally we drop the live
  // streams, the "thinking"/sending state, and the optimistic echo so the user
  // can send a new message right away rather than waiting for a stuck turn.
  async function handleStop() {
    try {
      await stopTurn(sessionId);
    } catch (e) {
      toast.error(`Couldn't stop: ${errMsg(e)}`);
    } finally {
      setStreams(new Map());
      setSending(false);
      setOptimisticUser(null);
      qc.invalidateQueries({ queryKey: ["chat", sessionId] });
    }
  }
  // A turn is in flight whenever we're sending or a stream is live.
  const busy = sending || streams.size > 0;

  // Stall watchdog: while a turn is in flight, flag it `stalled` if no delta has
  // arrived for ~40s. Cleared automatically when the turn ends (busy → false) or
  // resumes (a delta bumps lastActivityRef and clears the flag).
  useEffect(() => {
    if (!busy) {
      setStalled(false);
      return;
    }
    const id = window.setInterval(() => {
      if (Date.now() - lastActivityRef.current > 40_000) setStalled(true);
    }, 4000);
    return () => window.clearInterval(id);
  }, [busy]);

  const messages: ChatMessageDto[] = chat.data?.messages ?? [];
  // Retire the optimistic echo the moment the persisted user message lands in
  // the transcript — don't wait for the agent's stream to retire. Waiting made
  // the sent message double-render (persisted copy + optimistic bubble) for the
  // whole turn, and never de-dupe at all when the turn stalled (agent stuck on
  // "thinking"). The just-sent message is the newest, so match on the tail.
  useEffect(() => {
    if (!optimisticUser) return;
    const last = messages[messages.length - 1];
    if (last && last.role === "user" && last.body === optimisticUser) {
      setOptimisticUser(null);
    }
  }, [messages, optimisticUser]);
  // The only real provenance we have today is the chat's runtime (there's no
  // per-message model/provider on ChatMessageDto). Derive a single "via
  // provider/model" attribution line for agent turns from it.
  const runtimeVia = currentRuntime ? runtimePickerLabel(currentRuntime) : undefined;
  // Provenance line under an agent turn (§D11): the *runtime* only ("via
  // claude-code"). The model already rides the turn head (.tt-model), so the
  // attribution must not repeat it — "via claude-code / opus" said it twice.
  const runtimeProvider =
    currentRuntime?.provider?.trim() || currentRuntime?.label?.trim() || undefined;
  // Where the answering runtime runs — drives the avatar host badge (F5).
  const runtimeHost: "local" | "remote" =
    currentRuntime?.location?.trim().toLowerCase() === "remote" ? "remote" : "local";

  // The composer's resolved route (mockup §7.5): the first @mention in the draft
  // routes the turn; with none, it falls back to @primary (the chat's runtime).
  const routeMention = input.match(/@([\w-]+)/)?.[1]?.toLowerCase();
  // "@hive" and "@primary" both resolve to the default agent (no roster lookup).
  const routeIsPrimary =
    routeMention === "primary" || routeMention === PRIMARY_HANDLE;
  const routeHit =
    routeMention && !routeIsPrimary
      ? mentionCands.find((c) => c.handle.toLowerCase() === routeMention)
      : undefined;
  const routeHandle = routeHit?.handle ?? "primary";
  const routeVia = routeHit
    ? routeHit.handle === "all"
      ? "broadcast"
      : routeHit.kind
    : runtimeVia;
  // Streaming/pending turns are authored by the resolved recipient: the
  // @mentioned agent, else the default agent ("Hive" / @hive) — matching the
  // persisted turn so the head doesn't change identity when the stream lands.
  const streamAuthor = routeHandle !== "primary" ? routeHandle : PRIMARY_NAME;
  const streamHandle = routeHandle !== "primary" ? routeHandle : PRIMARY_HANDLE;

  async function selectRuntime(id: string) {
    setRouteOpen(false);
    await setChatRuntime(sessionId, id);
    qc.invalidateQueries({ queryKey: ["chat", sessionId] });
    qc.invalidateQueries({ queryKey: ["chats"] });
  }

  useEffect(() => {
    if (!routeOpen) return;
    const onDown = (e: MouseEvent) => {
      if (routeRef.current && !routeRef.current.contains(e.target as Node)) setRouteOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setRouteOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [routeOpen]);
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
        <div ref={scrollRef} onScroll={onScroll} className="flex flex-1 flex-col overflow-y-auto px-1 py-2">
          {chat.isLoading && messages.length === 0 && <SkeletonBubbles count={3} />}
          {chat.isError && messages.length === 0 && (
            <div className="max-w-md">
              <ErrorState
                text="Couldn't load this conversation."
                onRetry={() => void chat.refetch()}
              />
            </div>
          )}
          {!chat.isLoading && !chat.isError && messages.length === 0 && !optimisticUser && (
            <div
              className="max-w-lg rounded-3xl border p-6"
              style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
            >
              <div
                className="flex h-11 w-11 items-center justify-center rounded-2xl"
                style={{ background: "color-mix(in srgb, var(--hive-accent-cool) 18%, transparent)", color: "var(--hive-accent-cool)" }}
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
          {/* Anchor a short thread to the bottom (F18): a growing spacer pushes
              the turns down so two turns sit just above the composer, growing
              upward — rather than floating at the top over a dead gap. Only when
              turns exist, so the empty-state card keeps its top placement. */}
          {messages.length > 0 && <div className="flex-1" aria-hidden />}
          {messages
            // Hide a message's persisted copy while its live `streams` bubble is
            // still shown — not just mid-stream but through the completion
            // handoff (we keep the bubble until the refetch lands, above). The
            // stream bubble is the single source until it retires, so this never
            // renders the turn twice, and the swap is seamless (no anchor jump).
            .filter((m) => !streams.has(m.id))
            .map((m, idx, arr) => {
              // Day separator when the calendar day changes from the prior turn.
              const showDay =
                Boolean(m.createdAt) &&
                (idx === 0 || dayKey(m.createdAt) !== dayKey(arr[idx - 1]?.createdAt ?? ""));
              const rosterAgent = m.role === "user" || m.role === "system" ? undefined : agentByName.get(m.author);
              // A workspace-owned agent (empty ownerActorId) is a "shared" turn
              // (neutral tint); a personal agent keeps the cool agent tint and
              // takes its stored avatar colour as the turn accent.
              const shared = Boolean(rosterAgent) && rosterAgent!.ownerActorId.trim() === "";
              const turnColor = rosterAgent?.avatarColorHex ?? m.authorColorHex ?? undefined;
              // The mentionable handle for an agent turn: the roster name, else
              // the default agent's "@hive" (its author is stored as "Hive").
              const handle =
                m.role === "user" || m.role === "system"
                  ? undefined
                  : rosterAgent?.name ?? (m.author === PRIMARY_NAME ? PRIMARY_HANDLE : m.author);
              return (
                <Fragment key={m.id}>
                  {showDay && <DaySeparator label={dayLabel(m.createdAt)} />}
                  <Bubble
                  messageId={m.id}
                  role={m.role}
                  author={m.author}
                  handle={handle}
                  responderName={m.role === "assistant" ? m.responderName ?? undefined : undefined}
                  body={m.body}
                  createdAt={m.createdAt}
                  streaming={m.isStreaming}
                  via={runtimeProvider}
                  model={currentRuntime?.model || undefined}
                  host={runtimeHost}
                  shared={shared}
                  // Personal agents tint the turn with their own avatar colour;
                  // shared/human turns fall back to the theme (warm/muted/cool).
                  turnAccent={shared ? undefined : turnColor}
                  // Prefer the avatar stamped/resolved on the message; for the local
                  // user's own turns, fall back to their freshly-set local avatar so
                  // older turns aren't left blank before a re-stamp.
                  avatarUrl={
                    m.authorAvatarUrl ??
                    rosterAgent?.avatarUrl ??
                    (m.author === selfName ? selfAvatarUrl : undefined)
                  }
                  colorHex={rosterAgent?.avatarColorHex ?? m.authorColorHex}
                  reactions={m.reactions}
                  toolCalls={m.toolCalls}
                  toolResults={m.toolResults}
                  onReact={handleReact}
                  onRegenerate={m.id === lastAssistantId && !sending && streams.size === 0 ? handleRegenerate : undefined}
                  />
                </Fragment>
              );
            })}
          {optimisticUser && <Bubble role="user" author={selfName} body={optimisticUser} avatarUrl={selfAvatarUrl} />}
          {[...streams.entries()].map(([id, text]) => (
            <Bubble key={id} role="assistant" author={streamAuthor} handle={streamHandle} body={text} via={runtimeProvider} model={currentRuntime?.model || undefined} host={runtimeHost} streaming />
          ))}
          {sending && streams.size === 0 && !stalled && <TypingDots label={`${streamAuthor} is thinking`} />}
          {busy && stalled && (
            <div className="tt-note" role="status">
              <span aria-hidden className="shrink-0" style={{ flex: "0 0 auto" }}>
                <IconInfo size={14} />
              </span>
              <div className="min-w-0 flex-1">
                <div style={{ fontWeight: 650 }}>Still working…</div>
                <div className="mt-0.5 break-words opacity-80">
                  This is taking longer than usual — the runtime may be stalled or waiting to log in.
                </div>
              </div>
              <button
                onClick={() => void handleStop()}
                className="shrink-0 rounded-md px-2 py-0.5 text-xs font-semibold leading-none opacity-80 transition-opacity hover:opacity-100"
                style={{ border: "1px solid var(--hive-line)" }}
              >
                Stop &amp; retry
              </button>
            </div>
          )}
          {typingNames.length > 0 && <TypingDots label={typingLabel(typingNames)} />}
          {streamError && (
            <div className="tt-note bad" role="alert">
              <span aria-hidden className="shrink-0" style={{ flex: "0 0 auto" }}>
                <IconInfo size={14} />
              </span>
              <div className="min-w-0 flex-1">
                <div style={{ fontWeight: 650 }}>Generation failed or was interrupted</div>
                <div className="mt-0.5 break-words opacity-80">{streamError}</div>
              </div>
              <button
                onClick={() => setStreamError(null)}
                className="shrink-0 rounded-md px-1.5 py-0.5 leading-none opacity-60 transition-opacity hover:opacity-100"
                title="Dismiss"
                aria-label="Dismiss"
              >
                <IconX size={12} />
              </button>
            </div>
          )}
        </div>
          {hasNew && !atBottom && (
            <button
              onClick={scrollToBottom}
              className="absolute bottom-4 left-1/2 flex -translate-x-1/2 items-center gap-1.5 rounded-full px-3.5 py-1.5 text-xs font-semibold shadow-lg transition-transform hover:scale-105"
              style={{ background: "var(--hive-accent-fill)", color: "var(--hive-on-accent)" }}
            >
              <span aria-hidden><IconArrowDown size={13} /></span>
              New messages
            </button>
          )}
        </div>

        {/* The Claude-Code routing note lives in the route-pill popover (below),
            where routing is chosen — a standalone banner here just duplicated it
            and shoved the composer down mid-session (F9). */}
        <div className="border-t px-4 py-3.5" style={{ borderColor: "var(--hive-line)" }}>
          {/* Composer — a turn-shaped card; the "next turn" in the transcript
              (spec §7.5). Card lights its accent border on focus / while sending;
              the route pill resolves the recipient, and the @ / popovers stay. */}
          <div className={`cmp${composerFocused ? " is-focused" : ""}${sending ? " is-sending" : ""}`}>
            <div
              className="cmp-card"
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
              {attachments.length > 0 && (
                <div className="cmp-chips">
                  {attachments.map((a, i) => (
                    <span key={`${a.path}-${i}`} className="chip" title={a.path}>
                      <span aria-hidden>{a.image ? <IconImage size={12} /> : <IconFile size={12} />}</span>
                      <b>{a.name}</b>
                      <button
                        aria-label={`Remove attachment ${a.name}`}
                        onClick={() => setAttachments((cur) => cur.filter((_, j) => j !== i))}
                      >
                        <IconX size={10} />
                      </button>
                    </span>
                  ))}
                </div>
              )}

              <div className="cmp-in">
                {/* Composer autocomplete rides the shared Popover (R5) — backdrop
                    off so the textarea stays clickable; Escape / input-state dismiss. */}
                {slash && slashItems.length > 0 && (
                  <Popover anchorRef={taRef} backdrop={false} minWidth={300} onDismiss={() => setSlash(null)}>
                    {slashItems.map((c, i) => (
                      <PopoverItem
                        key={c.id}
                        label={c.label}
                        kind={c.hint}
                        active={i === slashActive}
                        onHover={() => setSlashActive(i)}
                        onSelect={() => c.run()}
                      />
                    ))}
                  </Popover>
                )}
                {mention && mentionMatches.length > 0 && (
                  <Popover anchorRef={taRef} backdrop={false} minWidth={280} onDismiss={() => setMention(null)}>
                    {mentionMatches.map((c, i) => (
                      <PopoverItem
                        key={c.handle}
                        label={<span className="font-medium">@{c.handle}</span>}
                        kind={c.kind}
                        active={i === mentionActive}
                        onHover={() => setMentionActive(i)}
                        onSelect={() => acceptMention(c.handle)}
                      />
                    ))}
                  </Popover>
                )}
                {emojiSc && emojiMatches.length > 0 && (
                  <Popover anchorRef={taRef} backdrop={false} minWidth={220} onDismiss={() => setEmojiSc(null)}>
                    {emojiMatches.map((c, i) => (
                      <PopoverItem
                        key={c.slug}
                        label={
                          <span>
                            <span className="mr-2 text-lg leading-none">{c.emoji}</span>
                            <span className="opacity-70">:{c.slug}:</span>
                          </span>
                        }
                        active={i === emojiScActive}
                        onHover={() => setEmojiScActive(i)}
                        onSelect={() => acceptEmojiShortcode(c.emoji)}
                      />
                    ))}
                  </Popover>
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
                  onFocus={() => setComposerFocused(true)}
                  onChange={(e) => {
                    const v = e.target.value;
                    const caret = e.target.selectionStart ?? v.length;
                    // `:shortcode:` → emoji on the closing colon (e.g. `:fire:` → 🔥).
                    const closingWord = closingShortcodeWord(v.slice(0, caret));
                    if (closingWord) {
                      const em = exactEmoji(emojiIndex, closingWord);
                      if (em) {
                        const scStart = caret - closingWord.length - 2;
                        const next = v.slice(0, scStart) + em + v.slice(caret);
                        setInput(next);
                        autoGrow();
                        setEmojiSc(null);
                        setSlash(null);
                        setMention(null);
                        requestAnimationFrame(() => {
                          const ta = taRef.current;
                          if (!ta) return;
                          const pos = scStart + em.length;
                          ta.setSelectionRange(pos, pos);
                        });
                        return;
                      }
                    }
                    setInput(v);
                    autoGrow();
                    const sl = detectSlash(v, caret);
                    setSlash(sl);
                    setSlashActive(0);
                    setMention(sl ? null : detectMention(v, caret));
                    setMentionActive(0);
                    // `:word` (2+ chars, at a word boundary) opens the emoji menu.
                    setEmojiSc(sl ? null : detectShortcode(v.slice(0, caret)));
                    setEmojiScActive(0);
                    if (v.trim()) pingTyping();
                    else clearTyping();
                  }}
                  onBlur={() => {
                    clearTyping();
                    setComposerFocused(false);
                    // Close autocomplete when focus leaves the composer. Picking an
                    // item preventDefaults its mousedown, so it never blurs here.
                    setSlash(null);
                    setMention(null);
                    setEmojiSc(null);
                  }}
                  onKeyDown={(e) => {
                    // While an IME is composing (CJK and other multi-keystroke
                    // input), Enter confirms the candidate — it must not send the
                    // message or drive the composer menus. `keyCode === 229` is the
                    // legacy signal for the same state.
                    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
                    // One shared reducer (↑↓ move · ↩/⇥ accept · esc dismiss) for the
                    // composer menus (R5).
                    if (
                      emojiSc &&
                      emojiMatches.length > 0 &&
                      candidateKey(e, {
                        count: emojiMatches.length,
                        active: emojiScActive,
                        setActive: setEmojiScActive,
                        onAccept: (i) => acceptEmojiShortcode(emojiMatches[i].emoji),
                        onDismiss: () => setEmojiSc(null),
                      })
                    )
                      return;
                    if (
                      slash &&
                      slashItems.length > 0 &&
                      candidateKey(e, {
                        count: slashItems.length,
                        active: slashActive,
                        setActive: setSlashActive,
                        onAccept: (i) => slashItems[i].run(),
                        onDismiss: () => setSlash(null),
                      })
                    )
                      return;
                    if (
                      mention &&
                      mentionMatches.length > 0 &&
                      candidateKey(e, {
                        count: mentionMatches.length,
                        active: mentionActive,
                        setActive: setMentionActive,
                        onAccept: (i) => acceptMention(mentionMatches[i].handle),
                        onDismiss: () => setMention(null),
                      })
                    )
                      return;
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      handleSend();
                    }
                  }}
                  rows={1}
                  placeholder="Message the room"
                  // Accessible name (the placeholder is not a reliable one) + signal
                  // the state of the @-mention / slash / emoji suggestion menus to
                  // assistive tech so a screen-reader user knows a popup is open.
                  aria-label="Message the room"
                  aria-haspopup="menu"
                  aria-expanded={Boolean(mention || slash || emojiSc)}
                />
              </div>

              <div className="cmp-f">
                {/* Route pill — the resolved recipient (spec §7.5). Shows the
                    draft's first @mention, else @primary + the chat runtime. The
                    popover switches which runtime is primary. */}
                <div className="rp-wrap" ref={routeRef}>
                  <button
                    type="button"
                    onClick={() => setRouteOpen((o) => !o)}
                    className={`rp${routeOpen ? " on" : ""}`}
                    title="Default recipient"
                    aria-expanded={routeOpen}
                  >
                    <span className="rp-ar" aria-hidden>→</span>
                    <b>@{routeHandle === "primary" ? PRIMARY_HANDLE : routeHandle}</b>
                    {routeVia && (
                      <>
                        <span className="rp-sep">·</span>
                        <span className="rp-via">via {routeVia}</span>
                      </>
                    )}
                    <span aria-hidden style={{ opacity: 0.55 }}>
                      <IconChevronDown size={11} />
                    </span>
                  </button>
                  {routeOpen && (
                    <div className="pop">
                      <div className="pop-h">Default recipient · when no @mention</div>
                      {runtimes.map((rt) => (
                        <button
                          key={rt.id}
                          className={`pop-i${rt.id === currentRuntimeId ? " sel" : ""}`}
                          // onMouseDown preventDefaults to keep the textarea from
                          // blurring on pointer select. That alone is mouse-only —
                          // a keyboard Enter/Space fires `click`, not `mousedown`,
                          // so also handle onClick (guarded so the mouse path, which
                          // already ran on mousedown, doesn't double-fire).
                          onMouseDown={(e) => {
                            e.preventDefault();
                            void selectRuntime(rt.id);
                          }}
                          onClick={(e) => {
                            if (e.detail === 0) void selectRuntime(rt.id);
                          }}
                        >
                          <span className="route-glyph"><IconWrench size={11} /></span>
                          <span className="pop-t">
                            <b>{rt.id === currentRuntimeId ? `@${PRIMARY_HANDLE}` : rt.label || rt.name}</b>
                            <span>{runtimePickerLabel(rt)}</span>
                          </span>
                          {rt.id === currentRuntimeId && (
                            <span aria-hidden style={{ color: "var(--hive-accent-fill)" }}>
                              <IconCheck size={12} />
                            </span>
                          )}
                        </button>
                      ))}
                      {currentRuntime?.provider === "claude-code" && (
                        <div className="pop-n">
                          Claude Code answers with its own tools &amp; permissions, via your Claude
                          subscription.
                        </div>
                      )}
                    </div>
                  )}
                </div>
                <span className="sp" />
                <div className="relative" ref={emojiPanelRef}>
                  <button
                    type="button"
                    className="ib"
                    title="Emoji"
                    aria-label="Insert emoji"
                    onClick={() => setShowEmoji((v) => !v)}
                  >
                    <IconSmile size={15} />
                  </button>
                  {showEmoji && (
                    <EmojiPicker
                      align="right"
                      onPick={(emoji) => {
                        insertEmoji(emoji);
                        setShowEmoji(false);
                      }}
                    />
                  )}
                </div>
                <label className="ib" title="Attach files" aria-label="Attach files">
                  <IconPaperclip size={15} />
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
                  className="ib"
                  style={{ width: "auto", padding: "0 8px", fontSize: 12 }}
                  title="Reference a workspace file (@file)"
                >
                  @file
                </button>
                {busy ? (
                  <button
                    type="button"
                    onClick={() => void handleStop()}
                    className="send"
                    aria-label="Stop generating"
                    title="Stop generating"
                  >
                    <IconStop size={15} />
                  </button>
                ) : (
                  <button
                    onClick={handleSend}
                    disabled={!input.trim() && attachments.length === 0}
                    className="send"
                    aria-label="Send message"
                    title="Send (Enter)"
                  >
                    <IconSend size={15} />
                  </button>
                )}
              </div>
            </div>

            {/* Footer hint (spec §7.5) — the affordance legend under the composer. */}
            <div className="hint">
              Live — type <span className="kbd">@</span> or <span className="kbd">/</span>, send with{" "}
              <span className="kbd">↵</span>, newline with <span className="kbd">⇧↵</span>.
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
      style={{ borderColor: "color-mix(in srgb, var(--hive-accent-cool) 26%, transparent)", background: "color-mix(in srgb, var(--hive-accent-cool) 10%, transparent)" }}
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

/// A centered date divider between turns on different calendar days.
function DaySeparator({ label }: { label: string }) {
  if (!label) return null;
  return (
    <div className="my-2 flex items-center gap-3 px-1 opacity-60" aria-label={label}>
      <div className="h-px flex-1" style={{ background: "var(--hive-line)" }} />
      <span className="text-[11px] font-medium uppercase tracking-wide">{label}</span>
      <div className="h-px flex-1" style={{ background: "var(--hive-line)" }} />
    </div>
  );
}

/// Renders a message: markdown text plus previews/chips for any `[Attached: …]`
/// paths. Image attachments show an inline thumbnail (loaded from the local
/// file as a data URL); non-images — and images whose bytes aren't on this
/// device — show a filename chip.
function MessageBody({ body }: { body: string }) {
  const { text, paths } = splitAttachments(body);
  return (
    <>
      {text && <Markdown content={text} />}
      {paths.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-2">
          {paths.map((p, i) =>
            isImagePath(p) ? (
              <ImageAttachment key={`${p}-${i}`} path={p} />
            ) : (
              <AttachmentChip key={`${p}-${i}`} path={p} />
            ),
          )}
        </div>
      )}
    </>
  );
}

/// A filename chip for a non-image attachment (or an image not available locally).
function AttachmentChip({ path, image }: { path: string; image?: boolean }) {
  return (
    <span
      className="inline-flex items-center gap-1.5 rounded-lg border px-2 py-1 text-xs"
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
      title={path}
    >
      <span aria-hidden className="opacity-60">
        {image ?? isImagePath(path) ? <IconImage size={13} /> : <IconFile size={13} />}
      </span>
      <span className="max-w-[14rem] truncate">{fileBaseName(path)}</span>
    </span>
  );
}

/// Inline image thumbnail. Loads the local file as a data URL; falls back to a
/// chip if the bytes aren't on this device (e.g. synced from another peer).
function ImageAttachment({ path }: { path: string }) {
  const img = useQuery({
    queryKey: ["image-attachment", path],
    queryFn: () => readImageDataUrl(path),
    staleTime: Infinity,
    retry: false,
  });
  if (img.isError) return <AttachmentChip path={path} image />;
  if (!img.data) {
    return (
      <div
        className="h-32 w-32 animate-pulse rounded-lg border"
        style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
        title={fileBaseName(path)}
      />
    );
  }
  return (
    <a href={img.data} target="_blank" rel="noreferrer" title={fileBaseName(path)}>
      <img
        src={img.data}
        alt={fileBaseName(path)}
        className="max-h-64 max-w-[min(20rem,100%)] rounded-lg border object-contain"
        style={{ borderColor: "var(--hive-line)" }}
      />
    </a>
  );
}

function prettyJson(s: string): string {
  try {
    return JSON.stringify(JSON.parse(s), null, 2);
  } catch {
    return s;
  }
}

/// A short single-line argument preview for a tool-call summary (the design's
/// dim `arg` slot) — the first scalar value in the JSON args, else the server.
function toolArgPreview(inputJson: string): string {
  try {
    const obj = JSON.parse(inputJson);
    if (obj && typeof obj === "object") {
      for (const v of Object.values(obj)) {
        if (typeof v === "string") return v;
        if (typeof v === "number" || typeof v === "boolean") return String(v);
      }
    }
  } catch {
    /* not JSON — fall through */
  }
  return "";
}

/// Inline tool-call activity: one dim line at rest, expanding to its arguments +
/// (matched-by-id) result. Exported for unit testing.
export function ToolCallCards({
  calls,
  results,
}: {
  calls: { id: string; name: string; inputJson: string; serverId: string | null }[];
  results: { callId: string; content: string; isError: boolean }[];
}) {
  return (
    <>
      {calls.map((c) => {
        const result = results.find((r) => r.callId === c.id);
        const arg = toolArgPreview(c.inputJson) || c.serverId || "";
        return (
          <details key={c.id} className="tcall">
            <summary className="tcall-h">
              <span aria-hidden className="tcall-chev">
                <IconChevronRight size={11} />
              </span>
              <span aria-hidden>
                <IconWrench size={12} />
              </span>
              <span className="tcall-n">{c.name}</span>
              {arg && <span className="tcall-a">{arg}</span>}
              {result && (
                <span className={`tcall-badge ${result.isError ? "bad" : "ok"}`}>
                  {result.isError ? "error" : "done"}
                </span>
              )}
            </summary>
            {c.inputJson && <pre className="tcall-o">{prettyJson(c.inputJson)}</pre>}
            {result && (
              <pre className={`tcall-o${result.isError ? " bad" : ""}`}>
                {result.content.length > 4000 ? `${result.content.slice(0, 4000)}…` : result.content}
              </pre>
            )}
          </details>
        );
      })}
    </>
  );
}


const Bubble = memo(function Bubble({
  messageId,
  role,
  author,
  handle,
  responderName,
  body,
  createdAt,
  streaming = false,
  via,
  model,
  host,
  shared = false,
  turnAccent,
  avatarUrl,
  colorHex,
  reactions,
  toolCalls,
  toolResults,
  onReact,
  onRegenerate,
}: {
  messageId?: string;
  role: string;
  author: string;
  /// The mentionable handle for an agent turn (rendered "@handle"); the human
  /// name (`author`) is shown as-is. Falls back to `author` when absent.
  handle?: string;
  /// For a primary-@hive turn: whose hive answered (shown as "· Alice"), so two
  /// same-named default agents are distinguishable.
  responderName?: string;
  body: string;
  createdAt?: string;
  streaming?: boolean;
  via?: string;
  model?: string;
  host?: "local" | "remote" | null;
  shared?: boolean;
  turnAccent?: string;
  avatarUrl?: string | null;
  colorHex?: string | null;
  reactions?: { emoji: string; actorId: string; actorDisplayName: string }[];
  toolCalls?: { id: string; name: string; inputJson: string; serverId: string | null }[];
  toolResults?: { callId: string; content: string; isError: boolean }[];
  onReact?: (messageId: string, emoji: string) => void;
  onRegenerate?: () => void;
}) {
  // One formatter, one timezone (§F3): relative age in-thread, absolute local
  // time on hover. `relTime`/`absTime` normalize a designator-less timestamp to
  // UTC first, so a stray naive `createdAt` can never render in the wrong zone
  // (which made a reply look ~8h earlier than the message it answered).
  // Show the wall-clock time (e.g. "3:42 PM") as the always-visible timestamp;
  // the full date + relative age are on hover.
  const timeLabel = createdAt ? clockTime(createdAt) : "";
  const timeTitle = createdAt ? `${absTime(createdAt)} (${relTime(createdAt)} ago)` : undefined;
  const isUser = role === "user";
  const isSystem = role === "system";
  const counts = new Map<string, number>();
  for (const r of reactions ?? []) counts.set(r.emoji, (counts.get(r.emoji) ?? 0) + 1);
  const [reactOpen, setReactOpen] = useState(false);
  const reactRef = useRef<HTMLDivElement>(null);
  // Close the emoji picker on outside click or Escape (the trigger button lives
  // inside reactRef, so clicking it still toggles rather than double-firing).
  useEffect(() => {
    if (!reactOpen) return;
    const onDown = (e: MouseEvent) => {
      if (reactRef.current && !reactRef.current.contains(e.target as Node)) setReactOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setReactOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [reactOpen]);

  const content = streaming ? (
    // Render Markdown progressively while streaming (not raw pre-wrap source), so
    // code fences / lists / headings appear formatted as they arrive instead of
    // popping into shape only when the turn retires. Markdown is memoized on
    // content, so each delta re-parses the accumulated text once; partial/unclosed
    // markup (an open ``` fence) renders gracefully. The caret trails the content.
    <div className="hive-stream">
      <MessageBody body={body} />
      <span className="tt-caret" />
    </div>
  ) : body ? (
    <MessageBody body={body} />
  ) : toolCalls && toolCalls.length > 0 ? null : (
    <span className="opacity-50">…</span>
  );

  // System messages (summaries, notices) read as centered meta, not a reply.
  if (isSystem) {
    return (
      <div className="tt-system">
        <span className="ln" />
        <span>{body}</span>
        {timeLabel && <span className="opacity-70">{timeLabel}</span>}
        <span className="ln" />
      </div>
    );
  }

  // Turn identity is carried by tint + avatar shape, never by side (always
  // left-aligned): warm = human, cool/agent-hue = personal agent, muted =
  // shared workspace agent. A personal agent overrides --turn-accent with its
  // own stored avatar colour; the theme still owns the fill/rail lightness.
  const kindClass = isUser ? "human" : shared ? "shared" : "agent";
  const rowStyle: CSSProperties = streaming
    ? {}
    // `auto` in the intrinsic size makes a row remember its measured height
    // after it has been on screen once, instead of snapping back to the 80px
    // guess when it scrolls away. Without that the container's scrollHeight
    // oscillates as you move through a long thread and never settles, so
    // autoscroll can't find a stable bottom to pin to (lib/autoscroll).
    : { contentVisibility: "auto", containIntrinsicSize: "auto 80px" };
  if (!isUser && !shared && turnAccent) {
    (rowStyle as Record<string, string>)["--turn-accent"] = turnAccent;
  }

  return (
    // Left-aligned turn row: a whisper of tint + avatar shape carry identity.
    // `content-visibility: auto` lets the engine skip layout/paint for off-screen
    // rows — cheap virtualization for long logs.
    <div className={`group tt ${kindClass}`} style={rowStyle}>
      <div className="tt-av">
        <Avatar
          name={author}
          url={avatarUrl}
          colorHex={colorHex}
          kind={isUser ? "human" : "agent"}
          host={isUser ? undefined : host}
          size={30}
        />
      </div>

      <div className="tt-body">
        <div className="tt-head">
          <span className={isUser ? "tt-name" : "tt-handle"}>
            {isUser ? author : `@${handle ?? author}`}
          </span>
          {responderName && (
            <span className="tt-badge" title={`Answered by ${responderName}'s hive`}>
              {responderName}
            </span>
          )}
          {shared && (
            <span className="tt-badge">
              <IconUsers size={10} /> workspace
            </span>
          )}
          {!isUser && model && <span className="tt-model">{model}</span>}
          {streaming && (
            <span className="tt-tag live" aria-label="generating">
              <i />
              <i />
              <i />
              generating
            </span>
          )}
          <span className="tt-spacer" />
          {timeLabel && <span className="tt-time" title={timeTitle}>{timeLabel}</span>}
        </div>
        {via && !isUser && <div className="tt-attr">via {via}</div>}

        <div className="tt-text">
          {content}
          {toolCalls && toolCalls.length > 0 && (
            <ToolCallCards calls={toolCalls} results={toolResults ?? []} />
          )}
        </div>

        {/* Persisted reactions stay visible; the copy / regenerate / react-picker
            row only appears on hover. */}
        <div className="tt-actions">
          {[...counts.entries()].map(([emoji, n]) => (
            <button key={emoji} onClick={() => onReact?.(messageId!, emoji)} className="react">
              {emoji} <span>{n}</span>
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
              <div className="relative" ref={reactRef}>
                <IconAction title="Add reaction" onClick={() => setReactOpen((o) => !o)}>
                  <IconSmile />
                </IconAction>
                {reactOpen && (
                  <EmojiPicker
                    onPick={(emoji) => {
                      onReact(messageId, emoji);
                      setReactOpen(false);
                    }}
                  />
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
