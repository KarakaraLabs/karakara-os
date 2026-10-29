# Changelog

## [0.3.0](https://github.com/KarakaraLabs/karakara-os/compare/v0.2.2...v0.3.0) (2026-10-29)


### Features

* **a2a:** fail-closed delegation with structured reply forwarding ([a11c3e4](https://github.com/KarakaraLabs/karakara-os/commit/a11c3e4d7910fc9ba2fae309a245885a308d98b1))
* add packaged agent identity digest ([e5d1ec9](https://github.com/KarakaraLabs/karakara-os/commit/e5d1ec99f790f5b130256fffe4201b99ac83887b))
* add TypeSafe System One contract spike ([b3d3f6e](https://github.com/KarakaraLabs/karakara-os/commit/b3d3f6e9d5288d50520621c590a17d98c7889f07))
* add typesafe system_one call controls ([b40125f](https://github.com/KarakaraLabs/karakara-os/commit/b40125fe033f9569e827d46f471896a76fe85c3b))
* **archive-read:** record-aware SearchRead, literal-default grep, and replay preservation ([3bb576a](https://github.com/KarakaraLabs/karakara-os/commit/3bb576a5c745f0503a85acbb5381259481e0d43b))
* attribute context events with kind, actor, and target ([c95615f](https://github.com/KarakaraLabs/karakara-os/commit/c95615f7ce7c41b7f2f116c3f5c332dd98a71ff8))
* **baml-derive:** emit wire-accurate JSON Schemas for static tool DTOs ([d2d9f6f](https://github.com/KarakaraLabs/karakara-os/commit/d2d9f6f4294c1532a4799a754317969de1d5f269))
* **baml-rt-builder:** add canonical ToolSchemaGraph and BAML renderer ([5a29e2e](https://github.com/KarakaraLabs/karakara-os/commit/5a29e2e77195b4f8d7746d6c5d9b5881c4f7f78d))
* **baml-rt-builder:** centralize TypeScript declaration ownership ([3d0e111](https://github.com/KarakaraLabs/karakara-os/commit/3d0e1117e061904c3427ddf93e0342b3e83bfcfc))
* **baml-rt-builder:** finish schema-driven TypeScript codegen ([3a95bf5](https://github.com/KarakaraLabs/karakara-os/commit/3a95bf5798a589601571ad5c568f879089ff1c24))
* **baml-rt-builder:** generate typed tool catalog and session API ([70104e5](https://github.com/KarakaraLabs/karakara-os/commit/70104e504331b03963625f1ca52d59d794fbb5f9))
* **baml-rt-builder:** make the schema graph own tool DTO generation ([6abe1a8](https://github.com/KarakaraLabs/karakara-os/commit/6abe1a8f596fb9cc164882d1d2aefe54db513aaf))
* **baml-rt-builder:** render tool schema graph to TypeScript ([7ca41fd](https://github.com/KarakaraLabs/karakara-os/commit/7ca41fd2a7ae04f3d9bfcd0496044edce491234d))
* **builder:** make the schema graph the sole tool declaration authority ([0f3997e](https://github.com/KarakaraLabs/karakara-os/commit/0f3997e2889aa7083de42a3d58aac86b70b80ee7))
* cut over provenance to stable agent identities ([f1bf61b](https://github.com/KarakaraLabs/karakara-os/commit/f1bf61bbc2dc24396cb8eea23c51641a09911f0d))
* derive agent identity at boot ([3379e27](https://github.com/KarakaraLabs/karakara-os/commit/3379e27c2243ab5003057b128c7cba6e0e80c5e8))
* enforce explicit A2A result handoff isolation (P3) ([3b37079](https://github.com/KarakaraLabs/karakara-os/commit/3b37079c8d86b0995db410fbe0d38992945eecd3))
* fence context-agent writes with provenance leases ([21d5950](https://github.com/KarakaraLabs/karakara-os/commit/21d595031394d5ab8f22eebda6c594529c02b56a))
* gate turns on context-agent lease ownership ([ff8a034](https://github.com/KarakaraLabs/karakara-os/commit/ff8a0346eb7219cde9d20f2af00179a6bb75e1d7))
* guard undeploy with pinned-context activity and recorded overrides ([f584a63](https://github.com/KarakaraLabs/karakara-os/commit/f584a63b75d540e7fcc9e76cdb0c873675dba229))
* hand off archived send results ([7d3d318](https://github.com/KarakaraLabs/karakara-os/commit/7d3d3181ea4681cec05a0b8218b190acf9f9358f))
* key agent package blobs by artifact hash ([7788e13](https://github.com/KarakaraLabs/karakara-os/commit/7788e13d031e7a063bf2d71f18d29354e0b66247))
* page agent context history through the retrieval tool ([f60a863](https://github.com/KarakaraLabs/karakara-os/commit/f60a863ef53b0638abb4991825cd942c07dcff77))
* pass session context to BamlTool execution ([897b1dd](https://github.com/KarakaraLabs/karakara-os/commit/897b1dd50b76a357697108cfd310036940f56fd9))
* pin context routes to a resolved agent and artifact ([16b25fc](https://github.com/KarakaraLabs/karakara-os/commit/16b25fc281a614764ba3e02076c76278cf78cef1))
* preserve caller context across delegation ([ce3c878](https://github.com/KarakaraLabs/karakara-os/commit/ce3c8784c0d0dad425f59136221d11b9f43add3c))
* read prompts from an agent-scoped context window ([3f6d5fc](https://github.com/KarakaraLabs/karakara-os/commit/3f6d5fc479be97dd2d7e81c48825c225a90636a5))
* record typed delegation lineage before dispatch ([314738d](https://github.com/KarakaraLabs/karakara-os/commit/314738daaa58e3da636340d02603e89aecc1fcd7))
* register TypeSafe System One tool contract ([05dc0bc](https://github.com/KarakaraLabs/karakara-os/commit/05dc0bc22096ba10029b013f96bfc879d4cee290))
* reserve agent versions before building artifacts ([c6dfe8c](https://github.com/KarakaraLabs/karakara-os/commit/c6dfe8c2c64962f733590ddb2e13d8e8cc681088))
* scope archives, receipts, and citations to the reading agent ([a9bc2d7](https://github.com/KarakaraLabs/karakara-os/commit/a9bc2d7ec5f7c0f75a14ceb930b0af2ef6766341))
* **semiotic:** gate trust UX, fast activity reads, and symmetric persistence ([8b68504](https://github.com/KarakaraLabs/karakara-os/commit/8b685048fbe7bbead2101ca4bb345322cca2e1e8))
* **semiotic:** gate trust UX, fast activity reads, and symmetric persistence ([3db5f7b](https://github.com/KarakaraLabs/karakara-os/commit/3db5f7bdbbbeef2d8b475a6b804d553c2c8ad606))
* split agent-prompt compaction from audit compaction (P4) ([768d277](https://github.com/KarakaraLabs/karakara-os/commit/768d277e9ff7dfdf847d6d7a24940f101dab80b9))
* store agent identity digest on repository entries ([7559df7](https://github.com/KarakaraLabs/karakara-os/commit/7559df780f56693fc822cf7901c43f60e5833c0d))
* **tools:** wire the typesafe feature through runner, api, builder, and links ([48619f1](https://github.com/KarakaraLabs/karakara-os/commit/48619f12857a12b3b56df4016a5d832a364ef73c))
* validate TypeSafe System One contracts ([e47ced5](https://github.com/KarakaraLabs/karakara-os/commit/e47ced5a032226df5e26792b8a7f171e6b5f11c9))
* verify agent identity and artifact across runners ([87bbe3b](https://github.com/KarakaraLabs/karakara-os/commit/87bbe3b1ed4fe52d43d9428e84548d595b7925b4))
* wire typesafe system_one config validation and scoped secrets ([35c0ffe](https://github.com/KarakaraLabs/karakara-os/commit/35c0ffef858de77c3b4b349d2d209d4d235c2d4c))


### Bug Fixes

* **baml-derive:** honor serde skips and variant renames ([1c544bb](https://github.com/KarakaraLabs/karakara-os/commit/1c544bbb10cc3bc76c85daf4d6a5aad354db6340))
* **baml-rt-builder:** canonicalize schema graph fingerprints ([9c796d0](https://github.com/KarakaraLabs/karakara-os/commit/9c796d054f3e431cc8f4165ec119f73ca5745366))
* **baml-rt-builder:** lower schema const values to literals ([b2f0db4](https://github.com/KarakaraLabs/karakara-os/commit/b2f0db4d352d3b58cb82f8169701c19be6e61dff))
* **baml-rt-builder:** map graph symbols to valid TypeScript names ([deddf91](https://github.com/KarakaraLabs/karakara-os/commit/deddf91ad110937f10cfd8c1a62025e230a5cdfd))
* **baml-rt-builder:** reject authored BAML shadowing runtime TS symbols ([8b7e35b](https://github.com/KarakaraLabs/karakara-os/commit/8b7e35b29d90f4f18dbb0a4f65405ffabf3a6dfb))
* **baml-rt-builder:** remap graph-owned IR symbols for TypeScript ([32df3d8](https://github.com/KarakaraLabs/karakara-os/commit/32df3d8841bd5a5adbafc8140e413c34ac78ec62))
* **baml-rt-builder:** validate schema graph cycles before BAML rendering ([49a1816](https://github.com/KarakaraLabs/karakara-os/commit/49a1816f3ca5ac719e9721efaf445b7b04321d85))
* **builder:** request typescript@5 in the npx tsc fallback ([956ba46](https://github.com/KarakaraLabs/karakara-os/commit/956ba46a9e0a97ae0fecd32b34f70736c320b66f))
* **ci:** add SPDX headers to gate provenance verifier script ([9210d4b](https://github.com/KarakaraLabs/karakara-os/commit/9210d4bf58286a52d0a20259fd05c3d70fd2cb52))
* **ci:** clippy and drift field renames after semiotic rebase ([7799ce2](https://github.com/KarakaraLabs/karakara-os/commit/7799ce216e7e4c16830bd3c4585ef87b9ec7d69a))
* **ci:** drop ONNX warmup steps after embedding crate removal ([b9c25c9](https://github.com/KarakaraLabs/karakara-os/commit/b9c25c9f1ded191c3b3e03b4bc5442f0131d913f))
* close agent identity review gaps ([747b0c9](https://github.com/KarakaraLabs/karakara-os/commit/747b0c9f76215dde0e784d2bd65f20014d75e07a))
* complete tool schema graph contracts ([6c4d0ee](https://github.com/KarakaraLabs/karakara-os/commit/6c4d0ee9f6d55c00228402f46d45ef523a5b4935))
* detect silent archive-read content loss in the step executor ([7054ca9](https://github.com/KarakaraLabs/karakara-os/commit/7054ca9e8f10bd2f63da4c7952161c89547b309a))
* keep same-context fanout transcript rows under concurrent writes ([744acdc](https://github.com/KarakaraLabs/karakara-os/commit/744acdc4522cef15dbf1443df693093fcb02420c))
* keep the latest agent prompt transcript tail ([0191ed6](https://github.com/KarakaraLabs/karakara-os/commit/0191ed6b2c277fbfc91590214e2273a339278686))
* key compaction heads by agent and bound compacted prompts ([b17279c](https://github.com/KarakaraLabs/karakara-os/commit/b17279c6762544c2a66894c4275dc1be523709e5))
* make agent packaging deterministic ([4ea6050](https://github.com/KarakaraLabs/karakara-os/commit/4ea6050c302b626098369fd527105d8a7ed9b9b1))
* make release publish and fork deployable ([1542f8a](https://github.com/KarakaraLabs/karakara-os/commit/1542f8af4c4d2a6a0ac8d13ed545b5e9536b3af2))
* merge projection ref tables instead of replacing them ([433ba87](https://github.com/KarakaraLabs/karakara-os/commit/433ba8770569f7f40e820c839a55985d4e4217f1))
* migrate legacy deployment rows without artifact hashes ([15869cc](https://github.com/KarakaraLabs/karakara-os/commit/15869cc2e48fba4e0a9b894d413d3bff7fd39e0c))
* migrate startup scripts to the unified agentium CLI ([eb7bc7d](https://github.com/KarakaraLabs/karakara-os/commit/eb7bc7da44e70cc2674b1d0400c9be896ed85288))
* point debug binary path and CLI invocations at karak ([7d67736](https://github.com/KarakaraLabs/karakara-os/commit/7d6773618c4e3c33115d7e3c573902687217027b))
* preserve tool schema graph semantics ([5afd844](https://github.com/KarakaraLabs/karakara-os/commit/5afd84490c49436823c4ce4006a05a10fee9920b))
* remove agent_instance_id from agent plan contracts ([5a90b34](https://github.com/KarakaraLabs/karakara-os/commit/5a90b34c3cb64ee44063cce64d3f4bbedeb0d703))
* restore coordinator-to-ClickUp delegation end to end ([894170c](https://github.com/KarakaraLabs/karakara-os/commit/894170c39afc4b03cab99e3537aa360d77df81eb))
* run coordinator debug recipe with debug binary ([5a39b67](https://github.com/KarakaraLabs/karakara-os/commit/5a39b67245471e01cc29202053b1c8cb39eeace7))
* **semiotic:** use async mutex in conformance tests for clippy ([b132caa](https://github.com/KarakaraLabs/karakara-os/commit/b132caad94e2176f2e32f0ebc3a584a3f6bae610))
* stop discovery loops on repeated archive reads ([1a48d42](https://github.com/KarakaraLabs/karakara-os/commit/1a48d42248b12a5904310ea2c48b55b23d4f2209))

## [0.2.2](https://github.com/KarakaraLabs/karakara-os/compare/v0.2.1...v0.2.2) (2026-07-10)


### Bug Fixes

* embed host compaction baml ([24d3ba2](https://github.com/KarakaraLabs/karakara-os/commit/24d3ba20c198e71c89bb4687793fce5986483c80))
* embed host compaction baml ([4c79545](https://github.com/KarakaraLabs/karakara-os/commit/4c79545bd22e99225aef59bcff4bae1ed79a57e9))

## [0.2.1](https://github.com/KarakaraLabs/karakara-os/compare/v0.2.0...v0.2.1) (2026-07-06)


### Bug Fixes

* stabilize runtime progress diagnose check ([bf96a6d](https://github.com/KarakaraLabs/karakara-os/commit/bf96a6dce942484cacc88f1d7117dff28771d817))

## [0.2.0](https://github.com/KarakaraLabs/karakara-os/compare/v0.1.3...v0.2.0) (2026-07-05)


### Features

* auto-finish one-shot sessions after entry Send ([b54b845](https://github.com/KarakaraLabs/karakara-os/commit/b54b845942679f4244ddb8c42bb76322c8582dca))
* entry-hop typed Send with tool_name literal ([d025bdb](https://github.com/KarakaraLabs/karakara-os/commit/d025bdb7282b96ec303d8ef47a28d58a9383b481))
* host-owned context compaction and planning transcript projection ([1cae9d5](https://github.com/KarakaraLabs/karakara-os/commit/1cae9d56976b65441b6a32c0c1a27c922a0880f9))
* **provenance:** make drift scoring opt-in, no embedding model by default ([49965ba](https://github.com/KarakaraLabs/karakara-os/commit/49965ba102148bf53e4af822818fd5b853a70e5e))
* **provenance:** make drift scoring opt-in, no embedding model by default ([5c60ebb](https://github.com/KarakaraLabs/karakara-os/commit/5c60ebb88c2e519263b5815ebd587a91bba8a92e))
* settlement-driven compaction with legacy path removal ([b4d7a03](https://github.com/KarakaraLabs/karakara-os/commit/b4d7a03380e21a51155f159117282d032c4cc9bc))
* settlement-driven context compaction with wire-ref validation ([5922fd2](https://github.com/KarakaraLabs/karakara-os/commit/5922fd24f95af289a11dcd648210b3ad76f6a72e))
* unify platform host and SDK under agentium binary ([5325fea](https://github.com/KarakaraLabs/karakara-os/commit/5325fea4f546143b47792554ef395862e3c1a1c1))
* unify platform host and SDK under agentium binary ([22256b5](https://github.com/KarakaraLabs/karakara-os/commit/22256b58fcada3bbf008fb4caf1c7a6cdff7bc36))
* validate compaction summary refs with in-trigger LLM retry ([dca0dbe](https://github.com/KarakaraLabs/karakara-os/commit/dca0dbe79101d7e17484c4cee3f3a52fbe548937))


### Bug Fixes

* align archive ledger context fields ([a4d36e0](https://github.com/KarakaraLabs/karakara-os/commit/a4d36e0a39ab6f1b37c727cf7ed6fc7a18b9bb83))
* align dispatch and compaction tests with main ingress fields ([255c87d](https://github.com/KarakaraLabs/karakara-os/commit/255c87d10ab695a2c6fe7a93068bb8907d428484))
* **ci:** build agentium with stable toolchain for k8s smoke and release dry-run ([4d7d20e](https://github.com/KarakaraLabs/karakara-os/commit/4d7d20ef3aacd65d5e0e598bfb1d02ff51021451))
* **ci:** install libdbus in sandbox-adapter job apt deps ([41d52dc](https://github.com/KarakaraLabs/karakara-os/commit/41d52dc8b356bee83e3d2b811489ff9d42604af9))
* **ci:** repair agentium path typos and install libdbus in CI runners ([22f113f](https://github.com/KarakaraLabs/karakara-os/commit/22f113f463a958c88e778073d0b5e4f6005c79e1))
* **ci:** restore test runner bin, Docker libdbus deps, eval clippy ([37df683](https://github.com/KarakaraLabs/karakara-os/commit/37df683bdd9aa59e6ce6921c6d2f52ca03897017))
* **ci:** stable toolchain for sandbox parity and earlier k8s agentium build ([79f849f](https://github.com/KarakaraLabs/karakara-os/commit/79f849f4683fdca860a15727960488d239b4c139))
* clippy and fmt ([cbb5824](https://github.com/KarakaraLabs/karakara-os/commit/cbb58246abf1e64c4a6a4dc3b7784e20b74cc8b8))
* explicit Finish/Abort on a closed session is a no-op ([7193a3b](https://github.com/KarakaraLabs/karakara-os/commit/7193a3bddb267533fe308305aae54a621b1a78fa))
* harden direct send step executor flow ([2dfeb86](https://github.com/KarakaraLabs/karakara-os/commit/2dfeb86554690c0b688ceab7a697c80b76c14f4a))
* hide one-shot lifecycle ops from tool catalog ([9b05b11](https://github.com/KarakaraLabs/karakara-os/commit/9b05b117faa0fe880c0329b2a760f606233b28a6))
* intent-level failures and no dangling session on auto-opened Send ([79bf9d7](https://github.com/KarakaraLabs/karakara-os/commit/79bf9d7721d352eff1507064e1e9d899233bcd7f))
* isolate MCP one-shot logical sessions ([ae3be8f](https://github.com/KarakaraLabs/karakara-os/commit/ae3be8f7067f1a0d34a53e88decdab157d03ff3a))
* **k8s:** drop duplicate serve arg from runner StatefulSet ([1461683](https://github.com/KarakaraLabs/karakara-os/commit/1461683befac6c8746768acc5846b0e24cee32d7))
* preserve BAML clients without stored LLM config ([507bb73](https://github.com/KarakaraLabs/karakara-os/commit/507bb7355b92f665f544a46f13c1c62db35d6d61))
* ship host compaction BAML in runner container image ([1902630](https://github.com/KarakaraLabs/karakara-os/commit/1902630f1ea109e4f64c279b03a6fa18c68c9bfc))
* **test:** use in-memory deployment state in runner lib unit tests ([8f9d876](https://github.com/KarakaraLabs/karakara-os/commit/8f9d8766d5b80c9edf0901f15705afd8d8bffd7a))
* update openapi snapshot for crate version 0.1.3 ([8e6434a](https://github.com/KarakaraLabs/karakara-os/commit/8e6434ac52a5ed03c1234afb3b576734b71528f7))

## [0.1.3](https://github.com/KarakaraLabs/karakara-os/compare/v0.1.2...v0.1.3) (2026-06-26)


### Bug Fixes

* test release please ([dbc9e95](https://github.com/KarakaraLabs/karakara-os/commit/dbc9e9536c0109f308d2a4b24df5fc95efc4cd43))
* test release please ([7e98204](https://github.com/KarakaraLabs/karakara-os/commit/7e9820486de39eae5af532e1b2416082bc10894f))
