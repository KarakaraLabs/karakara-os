# Migration to Karakara Labs

Status: in progress. This doc records what we know about moving agentium-os from the semioticAI fork to the Karakara Labs workspace, and it lists the points we still need to work through. Nothing here is legal advice.

## Context

semioticAI no longer exists. The people who built agentium-os want to keep working on it under Karakara Labs, a lab started with friends. The code is under Apache 2.0, so a fork, rebrand, and commercial use are all permitted. What is not automatic is the branding, the attribution obligations, and the ownership and confidentiality questions around code written while employed at semioticAI.

Short version: Apache 2.0 lets you fork this, rebrand it, and sell it. You do not need to erase the original authors, and you should not.

We are treating this as a fork plus rebrand, not a relicense. Apache 2.0 stays.

## Repo facts, verified 2026-10-16

- `LICENSE` is plain, unmodified Apache 2.0. No extra terms or addenda.
- `NOTICE` exists and reads:

  ```
  Agentium OS
  Copyright 2026 Semiotic AI, Inc.

  This product includes software developed at Semiotic AI, Inc.
  (https://semiotic.ai).

  Licensed under the Apache License, Version 2.0. See the LICENSE file in
  the root of this repository for the full license text.
  ```

- `LICENSES/Apache-2.0.txt` exists. The project follows the REUSE spec via `REUSE.toml` plus SPDX headers, and `reuse lint` runs as a CI gate.
- 973 files carry `SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.` and `SPDX-License-Identifier: Apache-2.0`.
- The git remote is `git@github.com:neithanmo/agentium-os.git`, so the fork already lives under a personal account, not the semioticAI org.

## What Apache 2.0 allows

- Use, modify, and distribute the code, including commercially.
- Build a closed-source product on top of it.
- Sell it, host it as a service, or charge for support and consulting.
- Keep your own changes proprietary if you want.

## What you must keep

- Include the Apache 2.0 license text with anything you distribute.
- Keep existing copyright and attribution notices in the source files. Do not strip the "Copyright semioticAI" headers.
- Keep the NOTICE file and its contents verbatim. Add a Karakara Labs attribution line alongside it, but do not rewrite or drop the original.
- Mark files you changed with a notice saying they were modified.
- Add your own copyright line for your modifications, for example "Copyright 2026 Karakara Labs". The original notices stay, and yours goes alongside them.

## Rename and branding

Section 6 of the license gives no right to use the original trademarks or product names. So:

- Renaming the project is necessary unless we own the "Agentium" name.
- Remove logos, product names, and marketing references to semioticAI / Agentium OS.
- We can say factually "originally derived from agentium-os by semioticAI". That is honest attribution and does not imply endorsement.
- Do not imply the original company endorses or runs this version.

Rename the brand, keep the legal attribution.

## How people profit from Apache forks

- Hosted or managed service (SaaS), often the strongest option.
- Support, consulting, and integration contracts.
- Open core: keep the core open and sell proprietary add-ons like enterprise features, dashboards, and security or compliance modules.
- Your own brand, docs, and community. That is what customers actually pay for, since anyone can copy the code.

## Risks to check before investing

1. Who owns your contributions. If you were an employee or contractor, your employment or IP assignment agreement probably made the company the owner of what you wrote. A dissolved company's assets usually pass to a liquidator, an acquirer, or its shareholders. Code already released under Apache 2.0 stays usable by anyone, including you, but the rights holder could close or relicense future work. If the repo is not fully public, or you reuse private code you wrote there, check this carefully.
2. Confidentiality. NDAs and confidentiality clauses may still bind you on non-public material: private repos, customer data, internal docs. Only build from what is actually in the public repo.
3. Trademarks. Search whether "Agentium" is registered anywhere before choosing a similar name, and make sure the new name is clear.
4. Dependencies. Check their licenses. Apache is permissive, but some dependencies may be GPL/AGPL or have other restrictions.
5. Leaked secrets. Scan the git history for API keys, credentials, or customer data before publishing the fork.
6. CLAs. If contributors signed a Contributor License Agreement, read what it granted.

## Practical checklist

1. Fork the repo and keep the LICENSE and NOTICE files.
2. Keep the original copyright headers and add ours.
3. Pick a new name, check for trademark conflicts, and replace logos and branding.
4. Add a README line like "Derived from agentium-os (Apache 2.0)."
5. Review our old employment and IP agreements.
6. Ideally, contact the former founders or whoever holds the company's assets and get short written confirmation that they are fine with the fork. That removes most of the risk.
7. Talk to an IP lawyer for an hour before launching commercially. It is cheap compared to the risk.

## Open items for the next session

Confirmed from the repo. No decision needed, just keep these files as they are.

- [x] License text is plain, unmodified Apache 2.0. Keep `LICENSE` and `LICENSES/Apache-2.0.txt`.
- [x] NOTICE file exists. Keep its contents verbatim, add a Karakara Labs line alongside.
- [x] 973 files carry `SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.`. These lines stay.
- [x] The repo already uses REUSE, so compliance is mechanical. Add Karakara Labs copyright lines and keep `reuse lint` in CI.

Still to decide or find out.

- [x] Pick the new product name: **karakara-os**. Registry availability confirmed (GitHub org `karakara-labs`, crates.io, npm all clear). Trademark register search still pending.
- [x] Choose the exact "modified by" and "Copyright 2026 Karakara Labs" header wording, and apply it to files we touch:

  ```
  SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
SPDX-FileCopyrightText: 2026 Karakara Labs
  SPDX-FileCopyrightText: 2026 Karakara Labs
  SPDX-License-Identifier: Apache-2.0
  ```
- [ ] Check dependency licenses across the workspace, including vendored deps.
- [ ] Scan git history for leaked secrets before making the fork public.
- [ ] Review employment and IP assignment agreements. Confirm Semiotic AI, Inc. owned our work as work-for-hire.
- [ ] Confirm what is public here versus what only existed in private repos or internal docs.
- [ ] Check for any CLA and what it granted.
- [ ] Find who holds Semiotic AI, Inc.'s assets after dissolution, whether liquidator, acquirer, or shareholders, and get written sign-off.
- [ ] Book an hour with an IP lawyer before any commercial launch.
- [ ] Create the Karakara Labs workspace or org and plan the repo move, redirects, and CI.

## Bottom line

The license is clean and covers the fork, rebrand, and sale. The obligations are narrow and, thanks to REUSE, mechanically enforceable. The license cannot settle two things, and those are the real work: who owns the "Agentium" name, and who holds Semiotic AI, Inc.'s copyright after dissolution. Those get resolved outside the repo, with the former founders or whoever holds the company's assets, plus an hour of a lawyer's time before we go commercial.
