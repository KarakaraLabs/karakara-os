# Repository API Contract

## Scope

This document defines repository contracts for publish/entries/blob behavior.

## Three Identities

The repository carries three different SHA-256 values. They answer different
questions and must not be substituted for one another.

| Value | Input | Question |
| --- | --- | --- |
| `content_hash` | Versioned manifest plus canonical source bundle | Which repository entry is this? |
| `identity_digest` → `AgentId` | Behavior-defining packaged content | Which agent source is this? |
| `artifact_hash` | Exact stored `.tar.gz` bytes | Which blob was deployed? |

All three are lowercase hex SHA-256 strings.

`content_hash = sha256(canonical_source_bundle)` covers the manifest after the
repository writes the assigned version, so it changes on every publish.

`identity_digest` excludes the repository version and manifest signature. It
covers the manifest projection, authored `src/**/*.ts`, authored and generated
`baml_src/**/*.baml`, the rendered tool-schema catalog, and
`session_plan_functions.json`. `AgentId` is derived from it as
`UUIDv5(AGENTIUM_AGENT_NAMESPACE, "urn:agentium:agent-id:" + identity_digest)`.
Republishing identical content keeps the same `AgentId`.

`artifact_hash` is the SHA-256 of the stored package bytes. Build the blob before
inserting the entry, key the blob by `artifact_hash`, and carry both values on
the entry in the same write.

## Publish Ownership Rule

Publish is source-first and server-owned:

1. Client sends source bundle: `POST /repository/publish`
2. Repository reserves the next `(agent_name, version)` and writes that version
   into the manifest.
3. Host/repository publish orchestrator builds the deployable artifact from the
   reserved source and derives `artifact_hash` plus `identity_digest`.
4. Built bytes are stored under `artifact_hash`, then the entry is inserted with
   the reserved version and both derived values.

A publish result returns `hash`, `artifact_hash`, `identity_digest`,
`deployable`, `version_ref`, and `generation`. Callers can deploy or pin the
exact artifact without rehashing. `deployable=false` means the entry is a
source-only repository record (for example a standalone `/publish` or a fork)
and the runner will reject it until it is republished through the build path.

Reservation consumption keeps the reservation row in place while the entry is
inserted, so a second publisher cannot reserve the same version in the gap. An
expired holder cannot consume the row, and the row is removed after the entry
write. Clients do not upload arbitrary blobs to establish publish provenance.

## HTTP Semantics (MVP)

- `400 Bad Request`: malformed payload, invalid hash format
- `404 Not Found`: requested entry/blob not found
- `409 Conflict`: `(agent_name, version)` conflict

## Required Repository Endpoints (MVP)

- `GET /repository/blobs/{hash}` — `{hash}` is the artifact hash
- `POST /repository/publish`
- `GET /repository/entries`
- `GET /repository/entries/{hash}`
- `GET /repository/entries?name=<name>&version=<version>`

## Tag Source of Truth

- Tags are defined in `manifest.json` only.
- Publish ingests `manifest.tags` into repository metadata.
- No standalone tag mutation endpoint in MVP.

## Search Ingestion Inputs

- `manifest_text` from manifest fields (`name`, `description`, `capabilities`, `tools`, `tags`)
- `source_text` from bounded extraction over source bundle files (best effort)
