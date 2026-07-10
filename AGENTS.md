# AGENTS.md

**At the start of every session, invoke the `orient` skill (`/orient`).**

Study the following before working in this project:

Orientation: `README.md`
Architecture: `ARCHITECTURE.md`
Methodology authoring: `docs/methodology-authoring-guide.md`
Interface contract: `docs/interface-contract.md`
Contribution conventions: `CONTRIBUTING.md`
Principles: [pentaxis93/principles](https://github.com/pentaxis93/principles)
Shared governance: [commons](https://github.com/tesserine/commons) — read everything in this repo

This project does not vendor agent skills in-repo. Resolve project skills from
your global installs under `~/.claude/skills` and `~/.codex/skills`.

**CLAUDE.md and AGENTS.md are the same file** — CLAUDE.md is a symlink to AGENTS.md. Edit AGENTS.md. Never break the symlink.

## Context injection

When `runa step` invokes an agent, it delivers a context injection as a
natural-language prompt on stdin. The context contains everything the agent
needs to execute the protocol without querying the store directly.

**Fields:**

- **protocol** — the name of the protocol being executed.
- **work_unit** — optional scoping identifier. Present when the protocol's
  inputs are partitioned by work unit; absent for unscoped protocols.
- **instructions** — the protocol's `PROTOCOL.md` content.
- **inputs** — valid artifact instances available to this execution. Each
  input carries:
  - `artifact_type` — the artifact type name
  - `instance_id` — the instance identifier (also the filename stem)
  - `display_path` — workspace-relative path to the artifact file
  - `content_hash` — `sha256:<hex>` digest of the artifact's canonical JSON
  - `relationship` — `requires` (hard dependency, guaranteed valid) or
    `accepts` (soft dependency, available but not required)
- **expected_outputs** — artifact type names the agent is expected to produce,
  split into:
  - `produces` — must be delivered. The protocol fails postconditions if any
    are missing or invalid after execution.
  - `may_produce` — optional. Validated if present, not required.

**Rendered prompt structure:**

The prompt organizes this context under headings: required inputs appear under
"What you've been given", accepted inputs under "Additional context", and
expected outputs under "What you need to deliver". The prompt instructs the
agent to call the MCP tool matching each output type name.

## Artifact production contract

Agents produce artifacts by calling tools on the `runa-mcp` MCP server. Each
protocol invocation gets its own single-session `runa-mcp` process.

**Tool derivation.** The server exposes one MCP tool per output artifact type
(`produces` and viable `may_produce` types). The tool name matches the artifact
type name.

**Tool input schema.** Each tool's input schema is derived from the artifact
type's JSON Schema with two modifications:
- When the effective schema requires `work_unit`, the field is removed from
  agent input and the server injects the delegated canonical work unit.
- When the effective schema declares optional `work_unit`, the field remains
  optional agent input. Omission means cross-cutting output; an explicit value
  must equal the delegated work unit and is preserved unchanged.
- `instance_id` is added as a required string field — the agent supplies this
  to name the artifact instance. It becomes the filename:
  `<workspace>/<type_name>/<instance_id>.json`.

**Validation.** Required scope is injected before complete validation. For an
optional declaration, runa validates the field's schema shape, checks its
authority against the delegated work unit, and then validates the complete
artifact. Malformed, foreign, and otherwise invalid artifacts are rejected
before either workspace or store persistence. If schema ownership cannot be
resolved safely, the output type fails closed instead of guessing.

**Postcondition enforcement.** After the agent process exits, `runa step`
re-scans the workspace and enforces postconditions: every `produces` artifact
type must have valid instances; `may_produce` artifacts are validated if present
but their absence is not a failure. A postcondition violation fails the protocol
execution.
