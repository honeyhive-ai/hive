/**
 * Companion service API — MC-002 §4, MC-001 §3/§5.
 *
 * Single source of truth for operation names and payloads, shared by the phone client and the
 * desktop service. Drift between the two should be a type error.
 *
 * This is a typed façade, not a proxy. The operations below are the complete allowlist. There is
 * no field here that becomes a filesystem path, a URL, or a command string, and no route that
 * forwards to the local runtime (unauthenticated plaintext HTTP on localhost).
 */

// ---------------------------------------------------------------------------
// Approvals — tier is assigned and enforced on the DESKTOP. The values below
// are what the desktop told us, for rendering only. Never computed on-device.
// ---------------------------------------------------------------------------

/** 1 = direct approve. 2 = hardware approval assertion + full body. 3 = view/defer only, desktop required. */
export type Tier = 1 | 2 | 3;

export type ApprovalKind =
  | 'conversational-gate'
  | 'continue'
  | 'file-diff'
  | 'command'
  | 'proposal';

export interface ApprovalSummary {
  id: string;
  kind: ApprovalKind;
  tier: Tier;
  /** BLAKE2s over the canonical record. Echoed back on decide; a mismatch means stale consent. */
  recordHash: string;
  workspaceId: string;
  createdAt: number;
  title: string;
}

export interface ApprovalRecord extends ApprovalSummary {
  /** The canonical body — the actual diff/command/gate text. Never an agent-authored summary. */
  body: CanonicalBody;
}

/**
 * What the human is shown, and nothing else.
 *
 * Deliberately carries no classification input. `tier` on ApprovalSummary is the desktop's whole
 * answer; a second, weaker signal next to it invites the phone to re-derive a verdict from it. The
 * removed `allInsideWorkspaceRoot` was the clearest case — containment is necessary but not
 * sufficient (`.git/hooks/pre-commit` satisfies it), so a `true` here read as "therefore Tier 2".
 * See spec/workspace-root-resolution.md.
 */
export type CanonicalBody =
  | { type: 'text'; content: string }
  /** `paths` is render content — the human must see which files — never a containment claim. */
  | { type: 'diff'; content: string; paths: string[] }
  | { type: 'command'; content: string }
  /** Over render budget or unrenderable. Defer-only at any tier — no approve control. */
  | { type: 'unrenderable'; reason: 'too-large' | 'binary' | 'unsupported-encoding' };

export type Decision = 'approve' | 'reject' | 'defer';

// ---------------------------------------------------------------------------
// Tier 2 assertion — amendment MC-002-A. See spec/device-approval-key.md.
//
// This replaces the deleted `biometricOk?: boolean`. That field asked the phone
// to self-report its own compliance, which the desktop could not verify; a build
// with the client checks stripped set it to `true` and Tier 2 became Tier 1.
// What follows is the same claim made unforgeable: a signature from a key the OS
// will not use without a fresh biometric match, checked against a public key
// registered at the desk. The desktop verifies rather than believes.
// ---------------------------------------------------------------------------

/**
 * ECDSA P-256 / SHA-256.
 *
 * NOT Ed25519, which the approved amendment text named: the iOS Secure Enclave
 * generates P-256 only (`kSecAttrTokenIDSecureEnclave`), so Ed25519 is
 * unimplementable on one of the two target platforms. P-256 is the curve both
 * the Enclave and Android StrongBox support. Recorded as an erratum in
 * DECISIONS.md § MC-002-A rather than changed silently, and ratified there on
 * 2026-08-10 — this is the binding algorithm, not a pending substitution.
 */
export type ApprovalKeyAlg = 'ecdsa-p256-sha256';

/** Where the private key actually lives. Recorded per device; shown in the audit view. */
export type ApprovalKeyBacking = 'secure-enclave' | 'strongbox' | 'tee';

/**
 * How much the desktop *knows* versus how much it took on trust.
 *
 * `attested` — an attestation chain proved the key's properties to the desktop (Android).
 * `ceremony` — the anchor is physical presence at the desk during SAS (iOS, which has no
 * equivalent to Android key attestation). These are not equal, and the registry keeps them
 * distinguishable so the audit view can say which one a given approval rests on.
 *
 * Shipping Tier 2 on both platforms with this distinction recorded per device was approved on
 * 2026-08-10, over the alternatives of Android-only Tier 2 and no Tier 2 in v1. What that vote
 * ruled out is rendering these two values as the same thing anywhere a human reads them.
 */
export type ApprovalKeyAssurance = 'attested' | 'ceremony';

/**
 * Desktop-issued freshness challenge, fetched immediately before signing.
 *
 * The nonce is issued by the desktop, not chosen by the phone. A phone-chosen nonce is a fine
 * replay guard and a useless freshness proof: it lets a client pre-sign a batch of approvals
 * while biometry is available and spend them later, which is session caching reintroduced
 * behind the platform flags that exist to forbid it.
 */
export interface ApprovalChallenge {
  approvalId: string;
  /** base64url. Single-use, bound to this device and this approval. */
  challenge: string;
  expiresAt: number;
}

/**
 * The Tier 2 proof, produced inside the OS biometric prompt.
 *
 * The signature covers a domain-separated transcript. Both sides reconstruct it — it is never
 * transmitted, so a client cannot sign one tuple and present another.
 *
 *   "hive-approval-v1" || canonical_cbor({
 *     id, tier, decision, record_hash, device_id, server_nonce, session_binding
 *   })
 *
 * `tier` is covered so a signature made for a Tier 2 record cannot be replayed against a record
 * re-classified since. `record_hash` binds it to the body actually rendered, and `decision` binds
 * it to approve/reject so a captured assertion cannot flip a reject into an approve.
 * `session_binding` is the Noise handshake hash, so a captured signature cannot be replayed into a
 * different session.
 *
 * No `keyId`: the registry holds exactly one approval key per device, so `deviceId` on the decide
 * params selects it. A client-supplied key identifier would only add a field the desktop must
 * ignore.
 */
export interface ApprovalAssertion {
  alg: ApprovalKeyAlg;
  /** The `challenge` from `approval.challenge`, echoed so the desktop can find the issued nonce. */
  challenge: string;
  /** base64url, IEEE P1363 (r || s), 64 bytes. */
  signature: string;
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

export interface Ops {
  'workspaces.list': { params: Record<string, never>; result: { workspaces: WorkspaceSummary[] } };
  'workspace.get': { params: { workspaceId: string }; result: WorkspaceDetail };

  'conversation.history': {
    params: { workspaceId: string; cursor?: string; limit?: number };
    result: { turns: Turn[]; cursor: string; hasMore: boolean };
  };
  'conversation.subscribe': { params: { workspaceId: string; cursor?: string }; result: StreamAck };

  'message.send': {
    params: { workspaceId: string; text: string; nonce: string };
    result: { turnId: string };
  };

  'approvals.list': { params: { workspaceId?: string }; result: { pending: ApprovalSummary[] } };
  'approval.get': { params: { id: string }; result: ApprovalRecord };
  /**
   * Desktop-issued, single-use, short-lived challenge for one Tier 2 decision on one device.
   * Separate op rather than a field on `approval.get` because records are cached (MC-001 §2) and a
   * cached challenge is an expired challenge.
   */
  'approval.challenge': {
    params: { id: string; nonce: string };
    result: ApprovalChallenge;
  };
  'approval.decide': {
    params: {
      id: string;
      decision: Decision;
      /** What the human actually saw. Desktop rejects if it no longer matches. */
      recordHash: string;
      deviceId: string;
      nonce: string;
      /**
       * REQUIRED for Tier 2 approvals. Missing, malformed or failing verification =>
       * `tier_forbidden`. Never a downgrade to Tier 1, never a soft accept.
       *
       * There is no boolean here and no password or PIN fallback: a fallback is the removed
       * `biometricOk` again with extra steps. A device that cannot register an approval key (no
       * biometric hardware, none enrolled, no secure element) has no Tier 2 — view/defer only.
       */
      approvalSig?: ApprovalAssertion;
    };
    result: { accepted: true };
  };

  'runs.list': { params: { workspaceId?: string }; result: { runs: RunStatus[] } };
  /** Idempotent, never gated, no biometric, no tier check that can fail closed. */
  'run.stop': { params: { runId: string; nonce: string }; result: { accepted: true } };

  'device.self': { params: Record<string, never>; result: DeviceEntry };
  /**
   * The phone reporting that its approval key is gone — invalidated by a biometric enrollment
   * change, or destroyed by the client on detecting one. The desktop clears `approvalKey` on the
   * registry row, so Tier 2 stops being offered to this device, and logs it as an audit event
   * (spec/device-approval-key.md §2).
   *
   * Report-only, and safe to accept unauthenticated-of-intent because it can **only remove**
   * authority: forging it buys an attacker a self-inflicted downgrade. There is deliberately no
   * inverse operation — re-binding an approval key requires the full pairing ceremony at the desk,
   * which is what stops an attacker who enrolled their own biometric from inheriting Tier 2.
   *
   * "Only removes" is a property of the whole system, not of this op alone, and it survives only
   * while the cleared state is never *more* permissive than the state before it. In particular the
   * desktop's refusal to accept approval-key registration over an established session must be
   * unconditional, never conditioned on the row already holding a key — otherwise this op becomes
   * the first half of the re-key path it exists to prevent
   * (spec/device-approval-key.md §2, spec/companion-service-api.md).
   *
   * The desktop also infers invalidation when a Tier 2 assertion fails to verify; it does not wait
   * to be told, because a tampered client simply would not send this.
   */
  'device.approvalKey.invalidate': {
    params: {
      reason: 'enrollment-changed' | 'destroyed-by-client' | 'unusable';
      nonce: string;
    };
    result: { accepted: true };
  };
  'push.register': {
    params: { token: string; platform: 'apns' | 'fcm'; nonce: string };
    result: { accepted: true };
  };
}

export type OpName = keyof Ops;
export type OpParams<K extends OpName> = Ops[K]['params'];
export type OpResult<K extends OpName> = Ops[K]['result'];

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

export interface WorkspaceSummary {
  id: string;
  name: string;
  agentCount: number;
  unreadCount: number;
  pendingApprovalCount: number;
  lastActivityAt: number;
}

export interface WorkspaceDetail extends WorkspaceSummary {
  participants: { name: string; type: 'human' | 'agent' | 'primary'; role?: string }[];
}

export interface Turn {
  id: string;
  cursor: string;
  /** Every turn is attributed — agent name, human name, or the primary runtime. */
  author: { name: string; type: 'human' | 'agent' | 'primary' };
  text: string;
  at: number;
}

export interface RunStatus {
  runId: string;
  workspaceId: string;
  label: string;
  state: 'running' | 'blocked' | 'done' | 'stopped' | 'failed';
  startedAt: number;
  /** Set when the run is waiting on an approval, so the inbox and run view agree. */
  blockedOnApprovalId?: string;
}

export interface DeviceEntry {
  deviceId: string;
  name: string;
  pairedAt: number;
  lastSeenAt: number;
  revoked: boolean;
  /**
   * MC-002-A. Absent = this device registered no approval key at pairing (no biometric hardware,
   * none enrolled, or no secure element) and therefore has **no Tier 2** — view/defer only.
   *
   * This is the desktop reporting its own registry back, not the phone describing itself. The
   * client renders from it; the desktop enforces from the same row regardless of what is rendered.
   */
  approvalKey?: {
    alg: ApprovalKeyAlg;
    backing: ApprovalKeyBacking;
    assurance: ApprovalKeyAssurance;
    registeredAt: number;
  };
}

export interface StreamAck {
  topic: Topic;
  cursor: string;
}

export type Topic = `conversation:${string}` | `approvals:${string}` | `runs:${string}`;

export type Event =
  | { topic: Topic; cursor: string; payload: { type: 'turn'; turn: Turn } }
  | { topic: Topic; cursor: string; payload: { type: 'run'; run: RunStatus } }
  | { topic: Topic; cursor: string; payload: { type: 'approval-created'; approval: ApprovalSummary } }
  | { topic: Topic; cursor: string; payload: { type: 'approval-decided'; id: string; decision: Decision } };

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

export interface Request<K extends OpName = OpName> {
  v: 1;
  op: K;
  params: OpParams<K>;
  nonce: string;
  ts: number;
}

export type Response<K extends OpName = OpName> =
  | { v: 1; ok: true; result: OpResult<K> }
  | { v: 1; ok: false; error: { code: ErrorCode; message: string } };

/** Typed codes, never free-form strings the phone parses. */
export type ErrorCode =
  | 'revoked'
  | 'replay'
  | 'stale_hash'
  | 'tier_forbidden'
  | 'not_pending'
  | 'cursor_too_old'
  | 'rate_limited'
  | 'unsupported_version'
  | 'internal';
