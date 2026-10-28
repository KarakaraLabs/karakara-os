// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** Config API DTOs (match backend ToolConfigSchemaDto, ToolConfigDto, etc.) */

export interface ToolConfigSchemaDto {
  tool_name: string;
  schema: Record<string, unknown>;
  default?: Record<string, unknown>;
  has_config: boolean;
}

export interface ToolConfigDto {
  tool_name: string;
  config: Record<string, unknown>;
  version: number;
}

export interface ConfigVersionDto {
  version: number;
  config: Record<string, unknown>;
  created_at_ms: number;
}

export interface SecretRequestDto {
  name: string;
  secret_type: string;
  justification: string;
  descriptor: string;
}

/** One secret in the M:N overview: which tools and LLM clients require it. */
export interface SecretOverviewEntryDto {
  name: string;
  secret_type?: string;
  justification?: string;
  descriptor?: string;
  tool_consumers: string[];
  llm_consumers: string[];
  /** True if provisioned; false when missing or when resolver unavailable. Always present. */
  satisfied: boolean;
  /** When satisfied and linked via UI: the key in the secret store this secret is linked to. */
  linked_to?: string;
}

/** Supported LLM provider IDs (must match backend LlmProvider enum). */
export const LLM_PROVIDERS = [
  "openai",
  "openai-base",
  "openai-generic",
  "openai-responses",
  "azure-openai",
  "ollama",
  "openrouter",
  "anthropic",
  "google-ai",
  "vertex",
  "aws-bedrock",
] as const;

export type LlmProvider = (typeof LLM_PROVIDERS)[number];

/** LLM config shape (bundle `llm`) — matches LlmClientConfig */
export interface LlmClientDef {
  name: string;
  provider: LlmProvider | string;
  options: Record<string, string>;
  retry_policy?: string;
}

export interface LlmOverrides {
  agent?: Record<string, string>;
  agent_function?: Record<string, string>;
}

export interface LlmRetryPolicyDef {
  max_retries?: number;
  strategy?: unknown;
}

export interface ModelBudgetOverride {
  context_window_tokens?: number;
  trigger_ratio?: number;
  emergency_ratio?: number;
  output_reserve_tokens?: number;
}

export interface CompactionBudgetDefaults {
  trigger_ratio?: number;
  emergency_ratio?: number;
  output_reserve_tokens?: number;
  recent_tail_retention?: number;
  item_threshold?: number;
  defer_while_in_flight?: boolean;
  defer_while_awaiting_input?: boolean;
}

export interface ModelBudgetSourceConfig {
  openrouter?: boolean;
  models_dev?: boolean;
  litellm?: boolean;
  cache_ttl_secs?: number;
}

export interface LlmCompactionConfig {
  defaults?: CompactionBudgetDefaults;
  model_overrides?: Record<string, ModelBudgetOverride>;
  client_overrides?: Record<string, ModelBudgetOverride>;
  online_sources?: ModelBudgetSourceConfig;
}

export interface LlmClientConfig {
  default: string;
  clients: Record<string, LlmClientDef>;
  overrides: LlmOverrides;
  retry_policies?: Record<string, LlmRetryPolicyDef>;
  compaction?: LlmCompactionConfig;
}

export type BudgetSource =
  | "configured"
  | "known"
  | "openrouter"
  | "models_dev"
  | "litellm"
  | "fallback";

export interface ModelContextBudget {
  model_id: string;
  provider: string;
  client_name: string;
  context_window_tokens: number;
  safe_prompt_tokens: number;
  emergency_prompt_tokens: number;
  output_reserve_tokens: number;
  source: BudgetSource;
  freshness: string;
  warning?: string | null;
}

export interface ResolvedClientBudgets {
  clients: ModelContextBudget[];
  refreshed_at_ms?: number | null;
}

export const LLM_BUNDLE_NAME = "llm";

export const KARAKARA_BUNDLE_NAME = "karakara";

export type KarakaraMode = "dry_run" | "enforce";

export interface KarakaraPolicy {
  enabled: boolean;
  mode: KarakaraMode;
  enforceMinTier: number;
  requirePostconditionsT3: boolean;
  strictCitationAnchors: boolean;
}

export interface KarakaraConfig extends KarakaraPolicy {
  overrides?: {
    agent?: Record<string, KarakaraPolicy>;
  };
}

export type KarakaraPosture = "off" | "audit" | "enforce";

export interface EffectiveSystemPolicy {
  policy: KarakaraPolicy;
  posture: KarakaraPosture;
  summary: string;
}

export interface EffectiveAgentPolicy {
  agentPackage: string;
  hasOverride: boolean;
  policy: KarakaraPolicy;
  posture: KarakaraPosture;
  summary: string;
}

export interface KarakaraEffectiveDto {
  version: number;
  system: EffectiveSystemPolicy;
  agents: EffectiveAgentPolicy[];
}

export interface AgentGateCounts {
  deny: number;
  ask: number;
  passGated: number;
  pass: number;
  frictionDenial: number;
  preventedError: number;
}

export interface RankedCount {
  code: string;
  count: number;
}

export interface GateIncidentDrill {
  contextId: string;
  taskId: string;
  toolCallAnchor: string;
}

export interface KarakaraIncidentRow {
  occurredAtMs: number;
  contextId: string;
  taskId: string;
  toolName: string;
  tier: number;
  decision: string;
  reasonCode: string;
  deficientNodes: string[];
  telemetryVerdict?: string;
  severity: "critical" | "warning" | "info";
  drill: GateIncidentDrill;
}

export interface KarakaraAgentActivityDto {
  agentPackage: string;
  effective: { posture: KarakaraPosture; summary: string };
  counts: AgentGateCounts;
  preventionRatio?: number | null;
  topReasonCodes: RankedCount[];
  topDeficientNodes: RankedCount[];
  recentIncidents: KarakaraIncidentRow[];
}

export interface KarakaraFleetActivityDto {
  denyCount: number;
  askCount: number;
  frictionDenialCount: number;
  preventedErrorCount: number;
  preventionRatio?: number | null;
  agentsWithActivity: number;
}

export interface KarakaraActivityDto {
  windowHours: number;
  sinceMs: number;
  untilMs: number;
  configVersion: number;
  fleet: KarakaraFleetActivityDto;
  emptyReason?: string | null;
  agents: KarakaraAgentActivityDto[];
}
