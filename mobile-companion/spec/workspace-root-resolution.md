# Workspace-root resolution

Implements MC-002 §2. Normative. Closes the gap named in [README](../README.md) as the
highest-risk one: the Tier 2 / Tier 3 boundary is a path-resolution problem, and path resolution
is where tier systems fail open.

## Two rules that govern everything below

**1. Containment is necessary, not sufficient.** "Every touched path resolves inside the workspace
root" is a *precondition* for Tier 2, not a definition of it. `.git/hooks/pre-commit` is inside the
workspace root and is arbitrary code execution on the owner's next commit. See
[Denied inside the root](#denied-inside-the-root).

**2. Reject, do not resolve.** The obvious design — canonicalise the path, then compare it to the
root — is the one that keeps failing in production. This spec instead *refuses* any path that
contains a construct requiring canonicalisation. A rejected path is not an error; it is Tier 3, and
Tier 3 means "walk to the desktop". That is a cheap price and it fails closed.

## Why lexical containment is not a check

Measured on the target platform (Windows 11), not assumed:

| Probe | Result |
|---|---|
| Junction inside root pointing outside | Path `…\work space\link\secret.txt` **starts with the root** and reads a file outside it |
| `[System.IO.Path]::GetFullPath` on `link\..\..\outside\secret.txt` | Collapses `..` **lexically**, ignoring that `link` is a reparse point |
| 8.3 short name for `work space` | `WORKSP~1` — a second, non-obvious spelling of the same directory |
| `a.txt.` (trailing dot) | `Test-Path` → **True**; Win32 strips it, so it aliases `a.txt` |
| `A.TXT` vs `a.txt` | **True** — case-insensitive, so a case-sensitive prefix compare fails open |
| Write to `a.txt:hidden` | Succeeds; `a.txt` now carries streams `:$DATA` and `hidden` |

The junction row is the whole argument. A string-prefix containment check returns *inside the
workspace* for a path that reads `C:\…\outside\secret.txt`.

The ADS row is the one that defeats the render rule rather than the path rule: the human approves a
diff to `a.txt`, and the bytes land in a stream that nothing displaying `a.txt` will ever show.

Note the divergence in the second row. On POSIX the kernel resolves `..` *per component*, so
`link/../x` depends on where `link` points; Win32 collapses `..` textually first. **The same diff
string denotes different files on different operating systems.** No cross-platform normalisation
routine can be trusted to paper over this, which is the third reason to reject rather than resolve.

## Path admission

A file diff is Tier 2 eligible only if **every** touched path — including both sides of every rename
and every deletion — passes all of the following. Any failure classifies the whole change Tier 3, by
the union rule in [approvals.md](./approvals.md).

### Stage 1 — form (no filesystem access)

Cheap, unambiguous, and evaluated before anything touches the disk.

- Path is **workspace-relative**. Absolute paths, drive-relative (`C:foo`), UNC (`\\server\share`)
  and device-namespace (`\\?\`, `\\.\`) forms are rejected outright.
- No NUL byte and no C0 control character.
- No `:` anywhere — this is both the ADS separator and the drive-relative marker.
- No component is empty, `.`, or `..`. **Traversal is rejected, not normalised.** A legitimate diff
  against a workspace has no reason to spell a path with `..`.
- No component has a trailing `.` or trailing space.
- No component matches a reserved device name (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`,
  `LPT1`–`LPT9`), with or without an extension.
- No component matches the 8.3 short-name shape `/~\d/`.
- No Unicode bidirectional control characters (`U+202A`–`U+202E`, `U+2066`–`U+2069`). These do not
  affect resolution; they attack the human reading the path, who is the final check.
- Normalise to **NFC** before any comparison. macOS returns NFD from the filesystem; comparing mixed
  forms silently mismatches.

### Stage 2 — walk (filesystem, component by component)

Resolve `realRoot` once at service start, and re-verify it on use — if the root is itself a symlink,
comparing resolved children against an unresolved root is inconsistent in both directions.

Walk the candidate's components outward from `realRoot`, one at a time:

- `lstat` each existing component. If any is a **symlink, junction, or reparse point of any kind —
  reject.** Do not follow it. Do not resolve it and compare. Reject.
- At the first non-existent component (the normal case for file creation), require that all
  remaining components also do not exist and have passed stage 1. Do not call `realpath` on a path
  that does not exist.
- If the final target exists, is a regular file, and has **link count > 1** — reject. A hardlink
  inside the workspace to a file outside it has a realpath *inside* the workspace and is invisible
  to every path-based check. Link count is the only signal available.

### Stage 3 — containment

Compare **component-wise**, never by string prefix: `/home/u/ws-evil` has `/home/u/ws` as a string
prefix and is a different directory. The path is contained if it equals `realRoot` or its component
sequence begins with `realRoot`'s.

Case sensitivity follows the filesystem, not the OS name: case-insensitive on Windows and on default
macOS, case-sensitive on Linux. Getting this backwards on a case-insensitive volume fails **open**.
Where the filesystem's behaviour is unknown, compare case-insensitively — that direction fails closed.

## Denied inside the root

Containment passed. These are still **Tier 3**, because each is a path to code execution or to
credential disclosure on a machine the phone's holder is not sitting at:

- `.git/**` — in particular `.git/hooks/**`, and `.git/config`, whose `core.pager`, `core.fsmonitor`
  and `alias.*` keys execute commands.
- `.hive/**` — runtime configuration and state.
- `.claude/**`, any `settings.json`, any `settings.local.json` — permissions and hooks, named
  explicitly by MC-002 §2.
- `.env`, `.env.*` — secrets, named explicitly by MC-002 §2.
- `package.json` — `scripts.preinstall` / `postinstall` / `prepare` run on the next install.
- `.github/workflows/**`, and equivalent CI definitions — code execution on push.
- `**/.bin/**`, `node_modules/**` — executables on `PATH` during ordinary development.
- Any change to file mode or the executable bit, at any path.

This list is a floor, owned by the classifier and versioned with it. Additions are not breaking
changes; removals are, and require the same review as an amendment to MC-002.

## Time of check, time of use

Classification happens when the record is created. Execution happens when a human decides, which may
be minutes later. In between, any component of any path can be replaced with a symlink.

- The **execution-time** check is the load-bearing one. Stage 1–3 run again immediately before the
  write, inside the same operation that performs it. The creation-time result drives UI only.
- If re-classification returns a different tier than the record was stamped with, the decision is
  **rejected** — not silently re-tiered — and the record's `record_hash` is invalidated so the phone
  re-fetches and the human re-consents. This reuses the stale-approval path already specified in
  [approvals.md](./approvals.md) §Decision flow rather than adding a second mechanism.
- Where the platform allows it, open by handle and write through the handle that was checked, rather
  than re-opening by path.

## Consequence for the API contract

[`contracts/api.ts`](../contracts/api.ts) modelled a diff body with
`allInsideWorkspaceRoot: boolean`. That field encoded the exact false equivalence this document
refutes — it read as "therefore Tier 2", and `.git/hooks/pre-commit` satisfies it.

**Applied.** It is removed, and `allowlisted: boolean` on the command body with it: same defect
class, since an allowlisted command is likewise a precondition being mistaken for a verdict. `tier`
is already on `ApprovalSummary`, is computed on the desktop, and is the only classification the
phone should ever see. A derived boolean alongside it invites a client to recompute a tier, which
MC-002 §2 forbids in its first sentence. `CanonicalBody` now carries render content only.

`paths: string[]` stays — the phone renders it — and is rendered with bidi controls escaped.
