// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! `karak serve` — start the Karakara platform.

use baml_agent_runner::RunnerCli;

pub fn run(cli: RunnerCli) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(baml_agent_runner::run(cli))
}
