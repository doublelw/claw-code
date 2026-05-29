use boa_engine::JsString;
use boa_engine::NativeFunction;
use boa_engine::Source;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowRunStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowResult {
    pub status: WorkflowRunStatus,
    pub output: String,
    pub agents_spawned: usize,
    pub agents_completed: usize,
    pub agents_failed: usize,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum WorkflowError {
    ScriptExecution(String),
    AgentLimitExceeded(usize),
    InvalidScript(String),
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScriptExecution(msg) => write!(f, "script execution error: {msg}"),
            Self::AgentLimitExceeded(max) => write!(f, "agent limit exceeded (max {max})"),
            Self::InvalidScript(msg) => write!(f, "invalid script: {msg}"),
        }
    }
}

impl std::error::Error for WorkflowError {}

pub struct WorkflowExecutionConfig {
    pub script: String,
    pub task_id: String,
    pub working_dir: PathBuf,
    pub env_vars: BTreeMap<String, String>,
    pub api_key: Option<String>,
    pub api_base_url: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Default)]
struct SharedState {
    output_buffer: String,
    agents_spawned: usize,
    agents_completed: usize,
    agents_failed: usize,
    agent_results: BTreeMap<String, String>,
    max_total_agents: usize,
    env_vars: BTreeMap<String, String>,
    api_key: Option<String>,
    api_base_url: String,
    model: String,
}

static EXECUTION_REGISTRY: OnceLock<Mutex<BTreeMap<String, Arc<Mutex<SharedState>>>>> =
    OnceLock::new();

fn registry() -> &'static Mutex<BTreeMap<String, Arc<Mutex<SharedState>>>> {
    EXECUTION_REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

static AGENT_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct WorkflowRuntime {
    max_concurrent_agents: usize,
    max_total_agents: usize,
}

impl WorkflowRuntime {
    pub fn new() -> Self {
        Self {
            max_concurrent_agents: 16,
            max_total_agents: 1000,
        }
    }

    pub fn with_limits(mut self, max_concurrent: usize, max_total: usize) -> Self {
        self.max_concurrent_agents = max_concurrent;
        self.max_total_agents = max_total;
        self
    }

    pub fn execute(
        &self,
        config: WorkflowExecutionConfig,
    ) -> Result<WorkflowResult, WorkflowError> {
        let shared = Arc::new(Mutex::new(SharedState {
            max_total_agents: self.max_total_agents,
            env_vars: config.env_vars.clone(),
            api_key: config.api_key.clone(),
            api_base_url: config
                .api_base_url
                .clone()
                .unwrap_or_else(|| "https://open.bigmodel.cn/api/coding/paas/v4".to_string()),
            model: config
                .model
                .clone()
                .unwrap_or_else(|| "GLM-4.7".to_string()),
            ..Default::default()
        }));

        // Register in global registry
        {
            let mut reg = registry().lock().unwrap();
            reg.insert(config.task_id.clone(), Arc::clone(&shared));
        }

        let mut context = boa_engine::Context::default();

        // Store task_id in a thread-local-like way through the registry
        let task_id_for_globals = config.task_id.clone();

        // Use fn pointers (no closures, no unsafe)
        context
            .register_global_callable(
                JsString::from("log"),
                1,
                NativeFunction::from_fn_ptr(js_log),
            )
            .map_err(|e| WorkflowError::ScriptExecution(e.to_string()))?;

        context
            .register_global_callable(
                JsString::from("spawnAgent"),
                2,
                NativeFunction::from_fn_ptr(js_spawn_agent),
            )
            .map_err(|e| WorkflowError::ScriptExecution(e.to_string()))?;

        context
            .register_global_callable(
                JsString::from("waitForAgent"),
                1,
                NativeFunction::from_fn_ptr(js_wait_for_agent),
            )
            .map_err(|e| WorkflowError::ScriptExecution(e.to_string()))?;

        context
            .register_global_callable(
                JsString::from("getEnv"),
                1,
                NativeFunction::from_fn_ptr(js_get_env),
            )
            .map_err(|e| WorkflowError::ScriptExecution(e.to_string()))?;

        // Inject task_id as a hidden global so fn pointers can find the right state
        context
            .register_global_property::<JsString, boa_engine::JsValue>(
                JsString::from("__claw_task_id"),
                JsString::from(task_id_for_globals).into(),
                boa_engine::property::Attribute::all(),
            )
            .map_err(|e| WorkflowError::ScriptExecution(e.to_string()))?;

        let source = Source::from_bytes(&config.script);
        let result = context.eval(source);

        // Cleanup registry
        {
            let mut reg = registry().lock().unwrap();
            reg.remove(&config.task_id);
        }

        match result {
            Ok(_) => {
                let state = shared.lock().unwrap();
                Ok(WorkflowResult {
                    status: WorkflowRunStatus::Completed,
                    output: state.output_buffer.clone(),
                    agents_spawned: state.agents_spawned,
                    agents_completed: state.agents_completed,
                    agents_failed: state.agents_failed,
                    error: None,
                })
            }
            Err(e) => {
                let state = shared.lock().unwrap();
                Ok(WorkflowResult {
                    status: WorkflowRunStatus::Failed,
                    output: state.output_buffer.clone(),
                    agents_spawned: state.agents_spawned,
                    agents_completed: state.agents_completed,
                    agents_failed: state.agents_failed,
                    error: Some(e.to_string()),
                })
            }
        }
    }
}

impl Default for WorkflowRuntime {
    fn default() -> Self {
        Self::new()
    }
}

fn get_task_id(context: &mut boa_engine::Context) -> Option<String> {
    context
        .global_object()
        .get(JsString::from("__claw_task_id"), context)
        .ok()
        .and_then(|v| v.as_string().map(|s| s.to_std_string_escaped()))
}

fn get_shared(task_id: &str) -> Option<Arc<Mutex<SharedState>>> {
    let reg = registry().lock().unwrap();
    reg.get(task_id).cloned()
}

fn js_log(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    context: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let Some(task_id) = get_task_id(context) else {
        return Ok(boa_engine::JsValue::undefined());
    };
    let Some(shared) = get_shared(&task_id) else {
        return Ok(boa_engine::JsValue::undefined());
    };
    let msg = args
        .first()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    let mut state = shared.lock().unwrap();
    if !state.output_buffer.is_empty() {
        state.output_buffer.push('\n');
    }
    state.output_buffer.push_str(&msg);
    Ok(boa_engine::JsValue::undefined())
}

fn js_spawn_agent(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    context: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let Some(task_id) = get_task_id(context) else {
        return Err(boa_engine::JsError::from_opaque(
            JsString::from("no task context").into(),
        ));
    };
    let Some(shared) = get_shared(&task_id) else {
        return Err(boa_engine::JsError::from_opaque(
            JsString::from("no shared state").into(),
        ));
    };
    let description = args
        .first()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    let prompt = args
        .get(1)
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();

    let (id, api_key, api_base_url, model) = {
        let mut state = shared.lock().unwrap();
        if state.agents_spawned >= state.max_total_agents {
            return Err(boa_engine::JsError::from_opaque(
                JsString::from(format!(
                    "agent limit exceeded (max {})",
                    state.max_total_agents
                ))
                .into(),
            ));
        }
        let id = format!("agent-{}", AGENT_COUNTER.fetch_add(1, Ordering::Relaxed));
        state.agents_spawned += 1;
        (
            id,
            state.api_key.clone(),
            state.api_base_url.clone(),
            state.model.clone(),
        )
    };

    // If API key is configured, call real LLM
    let result = if let Some(ref key) = api_key {
        call_llm(key, &api_base_url, &model, &description, &prompt)
    } else {
        // Mock mode
        Ok(format!("[Result from {description}]"))
    };

    let mut state = shared.lock().unwrap();
    match result {
        Ok(output) => {
            state.agent_results.insert(id.clone(), output);
            state.agents_completed += 1;
        }
        Err(e) => {
            state
                .agent_results
                .insert(id.clone(), format!("[Error: {e}]"));
            state.agents_failed += 1;
        }
    }
    Ok(JsString::from(id).into())
}

fn call_llm(
    api_key: &str,
    base_url: &str,
    model: &str,
    _description: &str,
    prompt: &str,
) -> Result<String, String> {
    use std::io::Read;

    let url = format!("{base_url}/chat/completions");
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "user", "content": prompt}
        ],
        "max_tokens": 2048,
        "temperature": 0.7
    });

    let response = ureq::post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send_json(&body)
        .map_err(|e| format!("API request failed: {e}"))?;

    let mut body_str = String::new();
    response
        .into_body()
        .as_reader()
        .read_to_string(&mut body_str)
        .map_err(|e| format!("read body failed: {e}"))?;

    let parsed: serde_json::Value =
        serde_json::from_str(&body_str).map_err(|e| format!("JSON parse failed: {e}"))?;

    parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("unexpected API response structure: {parsed}"))
}

fn js_wait_for_agent(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    context: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let Some(task_id) = get_task_id(context) else {
        return Err(boa_engine::JsError::from_opaque(
            JsString::from("no task context").into(),
        ));
    };
    let Some(shared) = get_shared(&task_id) else {
        return Err(boa_engine::JsError::from_opaque(
            JsString::from("no shared state").into(),
        ));
    };
    let agent_id = args
        .first()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    let state = shared.lock().unwrap();
    match state.agent_results.get(&agent_id) {
        Some(result) => Ok(JsString::from(result.clone()).into()),
        None => Err(boa_engine::JsError::from_opaque(
            JsString::from(format!("unknown agent: {agent_id}")).into(),
        )),
    }
}

fn js_get_env(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    context: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    let Some(task_id) = get_task_id(context) else {
        return Ok(boa_engine::JsValue::null());
    };
    let Some(shared) = get_shared(&task_id) else {
        return Ok(boa_engine::JsValue::null());
    };
    let key = args
        .first()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    let state = shared.lock().unwrap();
    match state.env_vars.get(&key) {
        Some(val) => Ok(JsString::from(val.clone()).into()),
        None => Ok(boa_engine::JsValue::null()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(script: &str) -> WorkflowExecutionConfig {
        static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);
        let uid = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        WorkflowExecutionConfig {
            script: script.to_string(),
            task_id: format!("test-{}-{}", uid, std::process::id()),
            working_dir: PathBuf::from("/tmp"),
            env_vars: BTreeMap::new(),
            api_key: None,
            api_base_url: None,
            model: None,
        }
    }

    fn test_config_with_api(script: &str) -> WorkflowExecutionConfig {
        static API_COUNTER: AtomicUsize = AtomicUsize::new(0);
        let uid = API_COUNTER.fetch_add(1, Ordering::Relaxed);
        let api_key = std::env::var("GLM_API_KEY").ok();
        WorkflowExecutionConfig {
            script: script.to_string(),
            task_id: format!("api-test-{}-{}", uid, std::process::id()),
            working_dir: PathBuf::from("/tmp"),
            env_vars: BTreeMap::new(),
            api_key,
            api_base_url: Some("https://open.bigmodel.cn/api/coding/paas/v4".to_string()),
            model: Some("GLM-4.7".to_string()),
        }
    }

    #[test]
    fn executes_simple_log_script() {
        let runtime = WorkflowRuntime::new();
        let result = runtime
            .execute(test_config("log(\"hello world\");"))
            .unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Completed);
        assert!(result.output.contains("hello world"));
    }

    #[test]
    fn spawn_agent_returns_id() {
        let runtime = WorkflowRuntime::new();
        let script = "const id = spawnAgent(\"test\", \"do work\"); log(id);";
        let result = runtime.execute(test_config(script)).unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Completed);
        assert!(result.output.contains("agent-"));
        assert_eq!(result.agents_spawned, 1);
    }

    #[test]
    fn wait_for_agent_returns_result() {
        let runtime = WorkflowRuntime::new();
        let script = "const id = spawnAgent(\"worker\", \"compute\"); const result = waitForAgent(id); log(result);";
        let result = runtime.execute(test_config(script)).unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Completed);
        assert!(result.output.contains("[Result from worker]"));
    }

    #[test]
    fn max_agents_enforced() {
        let runtime = WorkflowRuntime::new().with_limits(16, 2);
        let script = r#"
            const ids = [];
            for (let i = 0; i < 3; i++) {
                try {
                    ids.push(spawnAgent("w" + i, "task"));
                } catch(e) {
                    log("caught: " + e);
                }
            }
        "#;
        let result = runtime.execute(test_config(script)).unwrap();
        assert_eq!(result.agents_spawned, 2);
        assert!(
            result.output.contains("agent limit exceeded")
                || result.error.is_some()
                || result.output.contains("caught")
        );
    }

    #[test]
    fn script_syntax_error_returns_failed() {
        let runtime = WorkflowRuntime::new();
        let result = runtime.execute(test_config("function {")).unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Failed);
        assert!(result.error.is_some());
    }

    #[test]
    fn env_vars_accessible() {
        let mut config = test_config("log(getEnv(\"MY_VAR\"));");
        config
            .env_vars
            .insert("MY_VAR".to_string(), "test_value".to_string());
        let runtime = WorkflowRuntime::new();
        let result = runtime.execute(config).unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Completed);
        assert!(result.output.contains("test_value"));
    }

    #[test]
    fn get_env_returns_null_for_missing() {
        let runtime = WorkflowRuntime::new();
        let script = "const v = getEnv(\"MISSING\"); log(v === null ? \"null\" : v);";
        let result = runtime.execute(test_config(script)).unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Completed);
        assert!(result.output.contains("null"));
    }

    #[test]
    fn fan_out_multiple_agents() {
        let runtime = WorkflowRuntime::new();
        let script = r#"
            const agents = [];
            for (let i = 0; i < 5; i++) {
                agents.push(spawnAgent("w" + i, "task " + i));
            }
            const results = agents.map(id => waitForAgent(id));
            log("Collected " + results.length + " results");
        "#;
        let result = runtime.execute(test_config(script)).unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Completed);
        assert_eq!(result.agents_spawned, 5);
        assert_eq!(result.agents_completed, 5);
        assert!(result.output.contains("Collected 5 results"));
    }

    #[test]
    fn real_llm_single_agent() {
        let api_key = match std::env::var("GLM_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                eprintln!("skipping real LLM test: GLM_API_KEY not set");
                return;
            }
        };
        let runtime = WorkflowRuntime::new();
        let mut config = test_config_with_api("const id = spawnAgent(\"researcher\", \"What is 2+2? Reply with just the number.\"); const r = waitForAgent(id); log(r);");
        config.api_key = Some(api_key);
        let result = runtime.execute(config).unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Completed);
        assert_eq!(result.agents_spawned, 1);
        assert_eq!(result.agents_completed, 1);
        assert_eq!(result.agents_failed, 0);
        assert!(result.output.contains("4"));
    }

    #[test]
    fn real_llm_multi_agent_workflow() {
        let api_key = match std::env::var("GLM_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                eprintln!("skipping real LLM test: GLM_API_KEY not set");
                return;
            }
        };
        let runtime = WorkflowRuntime::new();
        let script = r#"
            const a1 = spawnAgent("writer", "Write a haiku about Rust programming language.");
            const a2 = spawnAgent("critic", "Write one sentence explaining why Rust is memory-safe.");
            const r1 = waitForAgent(a1);
            const r2 = waitForAgent(a2);
            log("Writer: " + r1);
            log("Critic: " + r2);
            log("Done: 2 agents completed");
        "#;
        let mut config = test_config_with_api(script);
        config.api_key = Some(api_key);
        let result = runtime.execute(config).unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Completed);
        assert_eq!(result.agents_spawned, 2);
        assert_eq!(result.agents_completed, 2);
        assert!(result.output.contains("Done: 2 agents completed"));
    }
}
