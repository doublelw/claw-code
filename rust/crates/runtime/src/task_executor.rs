use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Created,
    Running,
    Completed,
    Failed,
    Stopped,
}

#[derive(Debug)]
pub struct TaskHandle {
    pub id: String,
    pub command: String,
    pub started_at: Instant,
    pub status: TaskStatus,
    pub output_buffer: String,
    pub exit_code: Option<i32>,
}

pub struct TaskExecutor {
    tasks: HashMap<String, TaskHandle>,
    max_output_bytes: usize,
}

impl TaskExecutor {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            max_output_bytes: 1_000_000, // 1MB
        }
    }

    pub fn spawn(&mut self, id: &str, command: &str, cwd: Option<&str>) -> Result<(), String> {
        if self.tasks.contains_key(id) {
            return Err(format!("task already exists: {id}"));
        }

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;

        // Read output synchronously (in a real system this would be async)
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();

        if let Some(ref mut stdout) = child.stdout {
            let _ = stdout.read_to_end(&mut stdout_buf);
        }
        if let Some(ref mut stderr) = child.stderr {
            let _ = stderr.read_to_end(&mut stderr_buf);
        }

        let exit_status = child.wait().map_err(|e| format!("wait failed: {e}"))?;
        let exit_code = exit_status.code();

        let mut output = String::from_utf8_lossy(&stdout_buf).to_string();
        let stderr_str = String::from_utf8_lossy(&stderr_buf).to_string();
        if !stderr_str.is_empty() {
            output.push_str(&stderr_str);
        }

        // Truncate if too large
        if output.len() > self.max_output_bytes {
            output.truncate(self.max_output_bytes);
        }

        let status = if exit_code == Some(0) {
            TaskStatus::Completed
        } else {
            TaskStatus::Failed
        };

        self.tasks.insert(
            id.to_string(),
            TaskHandle {
                id: id.to_string(),
                command: command.to_string(),
                started_at: Instant::now(),
                status,
                output_buffer: output,
                exit_code,
            },
        );

        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&TaskHandle> {
        self.tasks.get(id)
    }

    pub fn list(&self) -> Vec<&TaskHandle> {
        self.tasks.values().collect()
    }

    pub fn stop(&mut self, id: &str) -> Result<TaskStatus, String> {
        if let Some(handle) = self.tasks.get_mut(id) {
            if handle.status == TaskStatus::Running {
                handle.status = TaskStatus::Stopped;
            }
            Ok(handle.status.clone())
        } else {
            Err(format!("task not found: {id}"))
        }
    }

    pub fn output(&self, id: &str) -> Option<&str> {
        self.tasks.get(id).map(|h| h.output_buffer.as_str())
    }

    pub fn remove(&mut self, id: &str) -> bool {
        self.tasks.remove(id).is_some()
    }
}

impl Default for TaskExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_echo_completes() {
        let mut executor = TaskExecutor::new();
        executor.spawn("t1", "echo hello", None).unwrap();
        let handle = executor.get("t1").unwrap();
        assert_eq!(handle.status, TaskStatus::Completed);
        assert_eq!(handle.exit_code, Some(0));
        assert!(handle.output_buffer.contains("hello"));
    }

    #[test]
    fn spawn_failing_command() {
        let mut executor = TaskExecutor::new();
        executor.spawn("t2", "exit 1", None).unwrap();
        let handle = executor.get("t2").unwrap();
        assert_eq!(handle.status, TaskStatus::Failed);
        assert_eq!(handle.exit_code, Some(1));
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut executor = TaskExecutor::new();
        executor.spawn("t3", "echo a", None).unwrap();
        assert!(executor.spawn("t3", "echo b", None).is_err());
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let executor = TaskExecutor::new();
        assert!(executor.get("nonexistent").is_none());
    }

    #[test]
    fn output_returns_buffer() {
        let mut executor = TaskExecutor::new();
        executor.spawn("t4", "echo test_output", None).unwrap();
        assert_eq!(executor.output("t4").unwrap().trim(), "test_output");
    }

    #[test]
    fn list_returns_all() {
        let mut executor = TaskExecutor::new();
        executor.spawn("a", "echo a", None).unwrap();
        executor.spawn("b", "echo b", None).unwrap();
        assert_eq!(executor.list().len(), 2);
    }

    #[test]
    fn remove_task() {
        let mut executor = TaskExecutor::new();
        executor.spawn("t5", "echo x", None).unwrap();
        assert!(executor.remove("t5"));
        assert!(executor.get("t5").is_none());
    }

    #[test]
    fn output_truncated_at_max() {
        let mut executor = TaskExecutor::new();
        executor.max_output_bytes = 100;
        executor.spawn("t6", "python3 -c \"print('x'*200)\"", None).unwrap();
        let output = executor.output("t6").unwrap();
        assert!(output.len() <= 100);
    }
}
