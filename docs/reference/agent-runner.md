# Agent Runner Reference

The `baml-agent-runner` crate provides the core A2A (Agent-to-Agent) host library that powers the Karakara OS runtime. It's instantiated via `karak serve` and handles agent lifecycle, conversation management, and tool execution.

## Configuration

### Drift Scoring (Opt-in)

Drift scoring for provenance tracking is now opt-in and requires explicit configuration:

```rust
// In config.rs - drift scoring is disabled by default
struct RunnerConfig {
    enable_drift_scoring: bool,  // Default: false
    embedding_model: Option<String>,  // Default: None
    // ... other config
}
```

To enable drift scoring:
1. Set `enable_drift_scoring: true` in runner configuration
2. Provide a valid `embedding_model` (e.g., `"text-embedding-3-small"`)
3. Ensure embedding provider credentials are available

### Embedding Model Configuration

The runner no longer assumes a default embedding model. When drift scoring is enabled, you must explicitly specify:

- **Model name**: Full provider/model identifier (e.g., `"openai/text-embedding-3-small"`)
- **Provider setup**: Ensure API keys are configured in `fnox.toml`
- **Fallback behavior**: If embedding fails, drift scoring is silently disabled for that session

## Core Components

### A2A Protocol

The runner implements the Agent-to-Agent JSON-RPC protocol over HTTP with Server-Sent Events (SSE) for real-time updates.

**Key endpoints:**
- `GET /agents` - Agent discovery (one entry per deployed package)
- `POST /agents/{agent_package}/a2a` - A2A JSON-RPC with SSE streaming
- `POST /agents/{agent_package}/dispatch` - Deterministic host-to-agent event delivery
- `GET /conversations/{id}` - Conversation state retrieval
- `POST /conversations` - New conversation creation

### Tool Session Management

Tools follow a finite state machine with these operations:
- **Open** - Initialize tool session
- **Send** - Execute tool with parameters
- **SearchRead** - Query tool results
- **PageRead** - Paginated result access
- **Finish** - Clean tool termination
- **Abort** - Force tool cleanup

### Cluster Mode

Cluster mode is activated by providing `--surreal-endpoint` (not `RUNNER_TOKEN` alone). In cluster mode:
- Shared SurrealDB instance for conversation persistence
- Distributed agent execution across multiple runner instances
- Consistent conversation state via graph database

### Provenance Integration

The runner integrates with `baml-rt-provenance` for conversation tracking:
- **Effect subscription**: Captures all conversation events
- **Graph persistence**: Stores conversation history in SurrealDB
- **Drift detection**: Optional embedding-based conversation drift scoring
- **Metrics emission**: OpenTelemetry metrics for conversation analytics

## Runtime Behavior

### Agent Identity

The runner derives a stable `AgentId` from the extracted package before building
the agent bridge. The identity digest covers the behavior-defining content
(manifest projection, authored TypeScript, BAML prompts and generated prelude,
tool-schema catalog, and session-plan dispatch table) and ignores repository
version, compiled `dist/**`, and package framing.

- The same packaged content keeps its `AgentId` across boots, replicas, and
  relocations.
- A behavior-defining source change produces a new `AgentId`.
- The `ArtifactHash` identifies the exact stored `.tar.gz` bytes. Two artifacts
  can share an `AgentId` and still execute different compiled output.

Boot verifies both values. A mismatch between the supplied `AgentId` or
`ArtifactHash` and the extracted package fails before the agent starts serving.
The `AgentBooted` event records the real process identity, not a placeholder.
Task execution carries that boot event ID explicitly, so provenance links a task
to the process lifetime and artifact that actually executed it. Ops rows resolve
`agent_version` only through that exact boot-to-artifact path; rows with no such
path omit the version and display the package alone. Startup restore also verifies
the persisted deployment artifact hash against the repository entry before
booting.

Cluster placements require an artifact hash. Forwarded A2A requests carry
`X-Agentium-Expected-Artifact-Hash`; the receiving runner compares it with the
exact `BootedAgent` selected for dispatch before creating a task, and returns
`412 Precondition Failed` on mismatch.
`AutoGenerate` identity is limited to ephemeral tests and development agents;
production boot paths pass the identity explicitly.

### Agent Lifecycle

1. **Package Resolution**: Load agent from repository or local path
2. **Build Pipeline**: Compile BAML → TypeScript → QuickJS bytecode
3. **Identity Derivation**: Hash the behavior set and derive `AgentId`
4. **Session Creation**: Initialize conversation context and tool sessions
5. **Execution**: Run agent code with tool access and conversation state
6. **Cleanup**: Terminate tool sessions and persist final state

### Error Handling

- **Tool failures**: Isolated per tool session, don't crash conversation
- **Agent crashes**: Captured and reported via conversation events
- **Network issues**: Retry logic for external tool calls
- **Resource limits**: Memory and execution time bounds enforced

### Performance Characteristics

- **Concurrent conversations**: Multiple conversations per runner instance
- **Tool parallelism**: Tools can execute concurrently within a conversation
- **Memory management**: QuickJS heap limits and garbage collection
- **Connection pooling**: Reused HTTP clients for external services

## Integration Points

### With baml-rt-api

The runner is embedded in the HTTP API layer (`baml-rt-api`) which provides:
- REST endpoints for conversation management
- Authentication and authorization middleware
- Request/response serialization
- Error response formatting

### With baml-rt-tools

Tool integration happens through:
- **Tool registry**: Dynamic tool discovery and instantiation
- **Session management**: Tool lifecycle coordination
- **MCP support**: Model Context Protocol tool providers
- **Host tools**: Built-in system tools (filesystem, network, etc.)

### With baml-rt-provenance

Provenance tracking provides:
- **Event capture**: All conversation events stored in graph database
- **Relationship modeling**: Conversations, messages, tool calls as graph nodes
- **Query capabilities**: Graph traversal for conversation analysis
- **Drift scoring**: Optional embedding-based conversation similarity

## Configuration Reference

### Environment Variables

- `RUNNER_TOKEN`: Authentication token for API access
- `SURREAL_ENDPOINT`: SurrealDB connection for cluster mode
- `BAML_TEST_MODEL`: Default LLM model for testing (default: `x-ai/grok-4.3`)
- `RUST_LOG`: Logging level configuration

### Config File Format

```toml
# fnox.toml - API keys and secrets
[openai]
api_key = "sk-..."

[anthropic]
api_key = "sk-ant-..."

# Runner-specific config
[runner]
enable_drift_scoring = false
embedding_model = "openai/text-embedding-3-small"
max_concurrent_conversations = 100
tool_timeout_seconds = 300
```

### Command Line Options

```bash
karak serve \
  --port 8080 \
  --surreal-endpoint ws://localhost:8000/rpc \
  --agent-path ./my-agent \
  --enable-drift-scoring \
  --embedding-model openai/text-embedding-3-small
```

## Debugging and Observability

### Logging

Structured logging with tracing spans:
- `conversation_id`: Tracks all events for a conversation
- `tool_session_id`: Isolates tool-specific operations
- `agent_package`: Identifies which agent is executing

### Metrics

OpenTelemetry metrics exported:
- `conversations_active`: Current active conversation count
- `tool_calls_total`: Total tool invocations by type
- `agent_execution_duration`: Agent execution time distribution
- `drift_score`: Conversation drift measurements (when enabled)

### Health Checks

- `GET /health`: Basic service health
- `GET /ready`: Readiness including database connectivity
- `GET /metrics`: Prometheus-format metrics endpoint
