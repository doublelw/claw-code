use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static RUN_COUNTER: AtomicU64 = AtomicU64::new(1);

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
pub struct WorkflowAgent {
    pub agent_id: String,
    pub description: String,
    pub status: WorkflowAgentStatus,
    pub output: Option<String>,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowAgentStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub script_name: String,
    pub status: WorkflowRunStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub agents: Vec<WorkflowAgent>,
    pub result_output: Option<String>,
    pub cached_results: HashMap<String, String>,
}

#[derive(Debug)]
pub enum OrchestratorError {
    NotFound(String),
    InvalidState(String),
    StartFailed(String),
}

impl std::fmt::Display for OrchestratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "workflow run not found: {id}"),
            Self::InvalidState(msg) => write!(f, "invalid state: {msg}"),
            Self::StartFailed(msg) => write!(f, "start failed: {msg}"),
        }
    }
}

impl std::error::Error for OrchestratorError {}

struct OrchestratorInner {
    runs: HashMap<String, WorkflowRun>,
}

pub struct WorkflowOrchestrator {
    inner: Arc<Mutex<OrchestratorInner>>,
}

impl WorkflowOrchestrator {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(OrchestratorInner {
                runs: HashMap::new(),
            })),
        }
    }

    pub fn start(
        &self,
        _script: &str,
        script_name: &str,
        _working_dir: &PathBuf,
    ) -> Result<String, OrchestratorError> {
        let run_id = format!("wf-{}", RUN_COUNTER.fetch_add(1, Ordering::Relaxed));
        let now = now_millis();

        let run = WorkflowRun {
            run_id: run_id.clone(),
            script_name: script_name.to_string(),
            status: WorkflowRunStatus::Running,
            created_at: now,
            updated_at: now,
            agents: Vec::new(),
            result_output: None,
            cached_results: HashMap::new(),
        };

        let mut inner = self.inner.lock().unwrap();
        inner.runs.insert(run_id.clone(), run);
        Ok(run_id)
    }

    pub fn pause(&self, run_id: &str) -> Result<(), OrchestratorError> {
        let mut inner = self.inner.lock().unwrap();
        let run = inner
            .runs
            .get_mut(run_id)
            .ok_or_else(|| OrchestratorError::NotFound(run_id.to_string()))?;

        match run.status {
            WorkflowRunStatus::Running => {
                run.status = WorkflowRunStatus::Paused;
                run.updated_at = now_millis();
                Ok(())
            }
            WorkflowRunStatus::Paused => Ok(()),
            other => Err(OrchestratorError::InvalidState(format!(
                "cannot pause run in {other:?} state"
            ))),
        }
    }

    pub fn resume(&self, run_id: &str) -> Result<(), OrchestratorError> {
        let mut inner = self.inner.lock().unwrap();
        let run = inner
            .runs
            .get_mut(run_id)
            .ok_or_else(|| OrchestratorError::NotFound(run_id.to_string()))?;

        match run.status {
            WorkflowRunStatus::Paused => {
                run.status = WorkflowRunStatus::Running;
                run.updated_at = now_millis();
                Ok(())
            }
            other => Err(OrchestratorError::InvalidState(format!(
                "cannot resume run in {other:?} state"
            ))),
        }
    }

    pub fn cancel(&self, run_id: &str) -> Result<(), OrchestratorError> {
        let mut inner = self.inner.lock().unwrap();
        let run = inner
            .runs
            .get_mut(run_id)
            .ok_or_else(|| OrchestratorError::NotFound(run_id.to_string()))?;

        match run.status {
            WorkflowRunStatus::Completed
            | WorkflowRunStatus::Failed
            | WorkflowRunStatus::Cancelled => Err(OrchestratorError::InvalidState(format!(
                "cannot cancel run in {:?} state",
                run.status
            ))),
            _ => {
                run.status = WorkflowRunStatus::Cancelled;
                run.updated_at = now_millis();
                Ok(())
            }
        }
    }

    pub fn complete(&self, run_id: &str, output: String) -> Result<(), OrchestratorError> {
        let mut inner = self.inner.lock().unwrap();
        let run = inner
            .runs
            .get_mut(run_id)
            .ok_or_else(|| OrchestratorError::NotFound(run_id.to_string()))?;

        run.status = WorkflowRunStatus::Completed;
        run.result_output = Some(output);
        run.updated_at = now_millis();
        Ok(())
    }

    pub fn fail(&self, run_id: &str, error: String) -> Result<(), OrchestratorError> {
        let mut inner = self.inner.lock().unwrap();
        let run = inner
            .runs
            .get_mut(run_id)
            .ok_or_else(|| OrchestratorError::NotFound(run_id.to_string()))?;

        run.status = WorkflowRunStatus::Failed;
        run.result_output = Some(error);
        run.updated_at = now_millis();
        Ok(())
    }

    pub fn get_run(&self, run_id: &str) -> Option<WorkflowRun> {
        let inner = self.inner.lock().unwrap();
        inner.runs.get(run_id).cloned()
    }

    pub fn list_runs(&self) -> Vec<WorkflowRun> {
        let inner = self.inner.lock().unwrap();
        inner.runs.values().cloned().collect()
    }

    pub fn add_agent(&self, run_id: &str, agent: WorkflowAgent) -> Result<(), OrchestratorError> {
        let mut inner = self.inner.lock().unwrap();
        let run = inner
            .runs
            .get_mut(run_id)
            .ok_or_else(|| OrchestratorError::NotFound(run_id.to_string()))?;
        run.agents.push(agent);
        run.updated_at = now_millis();
        Ok(())
    }
}

impl Default for WorkflowOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir() -> PathBuf {
        PathBuf::from("/tmp")
    }

    #[test]
    fn start_returns_run_id() {
        let orch = WorkflowOrchestrator::new();
        let id = orch.start("script", "test", &test_dir()).unwrap();
        assert!(id.starts_with("wf-"));
    }

    #[test]
    fn start_sets_running_status() {
        let orch = WorkflowOrchestrator::new();
        let id = orch.start("script", "test", &test_dir()).unwrap();
        let run = orch.get_run(&id).unwrap();
        assert_eq!(run.status, WorkflowRunStatus::Running);
        assert_eq!(run.script_name, "test");
    }

    #[test]
    fn list_runs_empty_initially() {
        let orch = WorkflowOrchestrator::new();
        assert!(orch.list_runs().is_empty());
    }

    #[test]
    fn list_runs_shows_started_runs() {
        let orch = WorkflowOrchestrator::new();
        orch.start("s1", "run1", &test_dir()).unwrap();
        orch.start("s2", "run2", &test_dir()).unwrap();
        assert_eq!(orch.list_runs().len(), 2);
    }

    #[test]
    fn pause_running_run() {
        let orch = WorkflowOrchestrator::new();
        let id = orch.start("script", "test", &test_dir()).unwrap();
        orch.pause(&id).unwrap();
        assert_eq!(orch.get_run(&id).unwrap().status, WorkflowRunStatus::Paused);
    }

    #[test]
    fn pause_already_paused_is_noop() {
        let orch = WorkflowOrchestrator::new();
        let id = orch.start("script", "test", &test_dir()).unwrap();
        orch.pause(&id).unwrap();
        orch.pause(&id).unwrap();
        assert_eq!(orch.get_run(&id).unwrap().status, WorkflowRunStatus::Paused);
    }

    #[test]
    fn resume_paused_run() {
        let orch = WorkflowOrchestrator::new();
        let id = orch.start("script", "test", &test_dir()).unwrap();
        orch.pause(&id).unwrap();
        orch.resume(&id).unwrap();
        assert_eq!(
            orch.get_run(&id).unwrap().status,
            WorkflowRunStatus::Running
        );
    }

    #[test]
    fn pause_nonexistent_fails() {
        let orch = WorkflowOrchestrator::new();
        assert!(orch.pause("nonexistent").is_err());
    }

    #[test]
    fn resume_nonexistent_fails() {
        let orch = WorkflowOrchestrator::new();
        assert!(orch.resume("nonexistent").is_err());
    }

    #[test]
    fn cancel_running_run() {
        let orch = WorkflowOrchestrator::new();
        let id = orch.start("script", "test", &test_dir()).unwrap();
        orch.cancel(&id).unwrap();
        assert_eq!(
            orch.get_run(&id).unwrap().status,
            WorkflowRunStatus::Cancelled
        );
    }

    #[test]
    fn cancel_completed_run_fails() {
        let orch = WorkflowOrchestrator::new();
        let id = orch.start("script", "test", &test_dir()).unwrap();
        orch.complete(&id, "done".to_string()).unwrap();
        assert!(orch.cancel(&id).is_err());
    }

    #[test]
    fn complete_sets_output() {
        let orch = WorkflowOrchestrator::new();
        let id = orch.start("script", "test", &test_dir()).unwrap();
        orch.complete(&id, "all done".to_string()).unwrap();
        let run = orch.get_run(&id).unwrap();
        assert_eq!(run.status, WorkflowRunStatus::Completed);
        assert_eq!(run.result_output, Some("all done".to_string()));
    }

    #[test]
    fn fail_sets_error() {
        let orch = WorkflowOrchestrator::new();
        let id = orch.start("script", "test", &test_dir()).unwrap();
        orch.fail(&id, "something broke".to_string()).unwrap();
        let run = orch.get_run(&id).unwrap();
        assert_eq!(run.status, WorkflowRunStatus::Failed);
        assert_eq!(run.result_output, Some("something broke".to_string()));
    }

    #[test]
    fn add_agent_to_run() {
        let orch = WorkflowOrchestrator::new();
        let id = orch.start("script", "test", &test_dir()).unwrap();
        let agent = WorkflowAgent {
            agent_id: "a1".to_string(),
            description: "worker".to_string(),
            status: WorkflowAgentStatus::Running,
            output: None,
            started_at: Some(now_millis()),
            completed_at: None,
        };
        orch.add_agent(&id, agent).unwrap();
        let run = orch.get_run(&id).unwrap();
        assert_eq!(run.agents.len(), 1);
        assert_eq!(run.agents[0].agent_id, "a1");
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let orch = WorkflowOrchestrator::new();
        assert!(orch.get_run("nonexistent").is_none());
    }

    #[test]
    fn default_is_empty() {
        let orch = WorkflowOrchestrator::default();
        assert!(orch.list_runs().is_empty());
    }
}
