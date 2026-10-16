# TypeSafe API reference fixtures

These fixtures close the API-reference gate for the Phase 1 `typesafe/system_one` tool.

| File | Evidence |
| --- | --- |
| `score_rounding.observation.json` | Result supplied from the TypeSafe Playground. The state, model, usage, and raw HTTP response were not available, so this is an observation rather than a wire fixture. |
| `structured_score.recorded.json` | Request and response published on the TypeSafe Score page for `jev-1.13.0`. It proves that structured levels are echoed in `legend`. |
| `null_choice.recorded.json` | Request excerpt and matching answer published on the TypeSafe Choice page. It proves that Choice descriptions may be `null`. |
| `choice_tie.contract.json` | Validation case derived from the Choice contract and Confidence page. The page defines an even split as a tie with confidence zero. This is not presented as a recorded model call. |
| `null_score.contract.json` | Request case derived from the Advanced structure page and JavaScript SDK `EntryType`. Both explicitly permit `null` Score levels. This is not presented as a recorded model call. |

Sources reviewed on 2026-10-16:

- <https://docs.typesafe.ai/api.md>
- <https://docs.typesafe.ai/models.md>
- <https://docs.typesafe.ai/primitives/choice.md>
- <https://docs.typesafe.ai/primitives/score.md>
- <https://docs.typesafe.ai/primitives/advanced.md>
- <https://docs.typesafe.ai/confidence.md>

The optional `scripts/capture_typesafe_system_one_fixtures.sh` script can replace contract-derived cases with authenticated responses later. Tests must preserve the distinction between recorded and derived evidence.
