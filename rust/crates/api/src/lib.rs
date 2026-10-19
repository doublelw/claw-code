mod client;
mod error;
mod http_client;
mod prompt_cache;
mod providers;
mod sse;
mod types;

pub use client::{
    oauth_token_is_expired, read_base_url, read_xai_base_url, resolve_saved_oauth_token,
    resolve_startup_auth_source, MessageStream, OAuthTokenSet, ProviderClient,
};
pub use error::ApiError;
pub use http_client::{
    build_http_client, build_http_client_or_default, build_http_client_with, ProxyConfig,
};
pub use prompt_cache::{
    CacheBreakEvent, PromptCache, PromptCacheConfig, PromptCachePaths, PromptCacheRecord,
    PromptCacheStats,
};
pub use providers::anthropic::{AnthropicClient, AnthropicClient as ApiClient, AuthSource};
pub use providers::openai_compat::{
    build_chat_completion_request, check_request_body_size, estimate_request_body_size,
    flatten_tool_result_content, is_reasoning_model, model_rejects_is_error_field,
    model_requires_reasoning_content_in_history, translate_message, OpenAiCompatClient,
    OpenAiCompatConfig,
};
pub use providers::{
    detect_provider_kind, max_tokens_for_model, max_tokens_for_model_with_override,
    model_family_identity_for, model_family_identity_for_kind, provider_diagnostics_for_model,
    resolve_model_alias, ProviderDiagnostics, ProviderKind,
};
pub use sse::{parse_frame, SseParser};
pub use types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent, MessageRequest,
    MessageResponse, MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent,
    ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};

pub use telemetry::{
    AnalyticsEvent, AnthropicRequestProfile, ClientIdentity, JsonlTelemetrySink,
    MemoryTelemetrySink, SessionTraceRecord, SessionTracer, TelemetryEvent, TelemetrySink,
    DEFAULT_ANTHROPIC_VERSION,
};

/// v2.1.292/296: read a backoff bound (milliseconds) from an environment
/// variable, falling back to the default when unset or unparsable. Used for
/// `CLAUDE_CODE_OVERLOADED_RETRY_BASE_DELAY_MS` and
/// `CLAUDE_CODE_OVERLOADED_RETRY_MAX_DELAY_MS`, which lengthen the backoff
/// applied when retrying overloaded (529) responses.
#[must_use]
pub fn env_backoff_ms(var: &str, default: std::time::Duration) -> std::time::Duration {
    std::env::var(var)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(default)
}

#[cfg(test)]
mod retry_env_tests {
    use super::env_backoff_ms;
    use std::time::Duration;

    #[test]
    fn env_backoff_ms_overrides_and_falls_back() {
        // v2.1.292/296: the overloaded-retry backoff bounds are env-tunable;
        // invalid values keep the built-in default.
        std::env::set_var("CLAUDE_CODE_OVERLOADED_RETRY_BASE_DELAY_MS", "5000");
        assert_eq!(
            env_backoff_ms(
                "CLAUDE_CODE_OVERLOADED_RETRY_BASE_DELAY_MS",
                Duration::from_secs(1)
            ),
            Duration::from_millis(5000)
        );
        std::env::set_var("CLAUDE_CODE_OVERLOADED_RETRY_BASE_DELAY_MS", "abc");
        assert_eq!(
            env_backoff_ms(
                "CLAUDE_CODE_OVERLOADED_RETRY_BASE_DELAY_MS",
                Duration::from_secs(1)
            ),
            Duration::from_secs(1)
        );
        std::env::remove_var("CLAUDE_CODE_OVERLOADED_RETRY_BASE_DELAY_MS");
        assert_eq!(
            env_backoff_ms(
                "CLAUDE_CODE_OVERLOADED_RETRY_BASE_DELAY_MS",
                Duration::from_secs(1)
            ),
            Duration::from_secs(1)
        );
    }
}
