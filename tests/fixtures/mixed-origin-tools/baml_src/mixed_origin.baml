// Fixture: BAML function signatures that overlap graph-owned tool DTOs.

class ExternalEchoInput {
  message string
}

class McpTypedInput {
  query string
}

function EchoExternal(input: ExternalEchoInput) -> string {
  input.message
}

function EchoMcp(input: McpTypedInput) -> string {
  input.query
}
