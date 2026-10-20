// Fixture: BAML function signatures that overlap graph-owned tool DTOs.

client MixedOriginClient {
  provider openai-generic
  options {
    model "gpt-4o-mini"
    base_url "https://example.invalid"
    api_key "test"
  }
}

class ExternalEchoInput {
  message string
}

class McpTypedInput {
  query string
}

class Foo_Bar {
  value string
}

function EchoExternal(input: ExternalEchoInput) -> string {
  client MixedOriginClient
  prompt #"{{ input }}"#
}

function EchoMcp(input: McpTypedInput) -> string {
  client MixedOriginClient
  prompt #"{{ input }}"#
}

function EchoHyphenated(input: Foo_Bar) -> string {
  client MixedOriginClient
  prompt #"{{ input }}"#
}
