// Hand-rolled LSP ⇄ Monaco adapter. Public surface: `LspAdapter.create(monaco)`
// builds the adapter (or returns null if discovery fails); the editor then calls
// attach/notifySave/detach/dispose across a model's lifecycle. See `manager.ts`.
export { LspAdapter } from "./manager";
export type { LspStatus, LspStatusKind } from "./manager";
