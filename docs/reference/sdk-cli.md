# SDK CLI Reference (moved)

The SDK CLI is now the unified **`karak`** binary. This page is a redirect and migration cheat sheet.

**Canonical reference:** [`karakara-cli.md`](karakara-cli.md)

## Migration

| Before | After |
|--------|-------|
| `cargo run -p cargo-agent-platform -- …` | `karakara …` or `cargo run -p karakara-os -- …` |
| `baml-agent-runner --serve-http …` | `karak serve --serve-http …` |
| `cargo-agent-platform push` | `karakara install agent` |
| `cargo-agent-platform build` / `regen` | removed — server builds on `POST /repository/publish`; use `just regen-fixtures` in the monorepo |
| `cargo-agent-platform deploy` | `karakara deploy --hash …` |
| `cargo-agent-platform publish` | `karakara publish --agent-dir …` |

Install and quick start: [`INSTALL.md`](../../INSTALL.md).
