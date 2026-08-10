/**
 * Companion transport interface — MC-002 §3.
 *
 * v1 ships LanTransport only. RelayTransport (v1.1) implements this same interface and
 * nothing above it changes. The Noise session is end-to-end between phone and desktop over
 * whatever carrier is in use; a transport moves opaque frames and cannot read them.
 */

import type { OpName, OpParams, OpResult, Event, Topic } from './api';

export type TransportKind = 'lan' | 'relay';

/** An address worth attempting a handshake against. Never a trust statement. */
export interface Candidate {
  kind: TransportKind;
  /** host:port for LAN, rendezvous id for relay. */
  address: string;
  /** From mDNS TXT or the pairing QR. A hint for ordering attempts, not for authentication. */
  fingerprintHint?: string;
  source: 'mdns' | 'qr-hint' | 'last-known' | 'relay-directory';
}

export interface DeviceKeys {
  /** Opaque handle to the Keychain/Keystore-held static private key. Never raw bytes. */
  staticKeyRef: string;
  /** Desktop static X25519 public key, pinned at pairing. The only thing that decides trust. */
  pinnedPeerStaticPk: Uint8Array;
  deviceId: string;
}

export interface SessionInfo {
  /** Informational only — for a status indicator. Must not gate any security decision. */
  transport: TransportKind;
  deviceId: string;
  since: number;
}

export interface SecureSession {
  readonly info: SessionInfo;
  /** The only write path. Transports expose no raw send. */
  request<K extends OpName>(op: K, params: OpParams<K>): Promise<OpResult<K>>;
  subscribe(topic: Topic, cursor?: string): AsyncIterable<Event>;
  close(reason?: string): Promise<void>;
  readonly closed: Promise<{ reason: string }>;
}

export interface CompanionTransport {
  readonly kind: TransportKind;
  /** Yields candidates as they are found; caller races them and cancels the losers. */
  discover(signal: AbortSignal): AsyncIterable<Candidate>;
  /** Resolves only on a completed Noise IK handshake against `keys.pinnedPeerStaticPk`. */
  connect(candidate: Candidate, keys: DeviceKeys, signal: AbortSignal): Promise<SecureSession>;
}
