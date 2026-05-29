use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspServerStatus {
    Stopped,
    Starting,
    Running,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct LspServerHandle {
    pub language: String,
    pub command: String,
    pub status: LspServerStatus,
}

pub struct LspOrchestrator {
    servers: HashMap<String, LspServerHandle>,
}

impl LspOrchestrator {
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }

    pub fn register(&mut self, language: &str, command: &str) {
        self.servers.insert(
            language.to_string(),
            LspServerHandle {
                language: language.to_string(),
                command: command.to_string(),
                status: LspServerStatus::Stopped,
            },
        );
    }

    pub fn start(&mut self, language: &str) -> Result<(), String> {
        let handle = self
            .servers
            .get_mut(language)
            .ok_or_else(|| format!("no LSP server registered for: {language}"))?;

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&handle.command);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        match cmd.spawn() {
            Ok(mut child) => {
                // Send initialize request
                let init_request = format!(
                    "Content-Length: {}\r\n\r\n{}",
                    init_request_body(language).len(),
                    init_request_body(language)
                );

                if let Some(ref mut stdin) = child.stdin {
                    let _ = stdin.write_all(init_request.as_bytes());
                    let _ = stdin.flush();
                }

                // Read response (simplified - in production would be async)
                if let Some(ref mut stdout) = child.stdout {
                    let reader = std::io::BufReader::new(stdout);
                    for line in reader.lines() {
                        if let Ok(line) = line {
                            if line.contains("\"result\"") {
                                handle.status = LspServerStatus::Running;
                                return Ok(());
                            }
                        }
                    }
                }

                handle.status = LspServerStatus::Failed("no init response".to_string());
                Err("LSP server initialization failed".to_string())
            }
            Err(e) => {
                handle.status = LspServerStatus::Failed(e.to_string());
                Err(format!("failed to start LSP server: {e}"))
            }
        }
    }

    pub fn stop(&mut self, language: &str) -> Result<(), String> {
        let handle = self
            .servers
            .get_mut(language)
            .ok_or_else(|| format!("no LSP server for: {language}"))?;
        handle.status = LspServerStatus::Stopped;
        Ok(())
    }

    pub fn get(&self, language: &str) -> Option<&LspServerHandle> {
        self.servers.get(language)
    }

    pub fn list(&self) -> Vec<&LspServerHandle> {
        self.servers.values().collect()
    }
}

impl Default for LspOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

fn init_request_body(language: &str) -> String {
    let id = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"initialize","params":{{"processId":null,"rootUri":null,"capabilities":{{}},"clientInfo":{{"name":"claw","version":"0.1"}},"locale":"{language}"}}}}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_list() {
        let mut orch = LspOrchestrator::new();
        orch.register("rust", "rust-analyzer");
        orch.register("typescript", "typescript-language-server --stdio");
        assert_eq!(orch.list().len(), 2);
    }

    #[test]
    fn get_registered_server() {
        let mut orch = LspOrchestrator::new();
        orch.register("rust", "rust-analyzer");
        let handle = orch.get("rust");
        assert!(handle.is_some());
        assert_eq!(handle.unwrap().command, "rust-analyzer");
    }

    #[test]
    fn get_unregistered_returns_none() {
        let orch = LspOrchestrator::new();
        assert!(orch.get("python").is_none());
    }

    #[test]
    fn start_unregistered_fails() {
        let mut orch = LspOrchestrator::new();
        assert!(orch.start("unknown").is_err());
    }

    #[test]
    fn stop_sets_status() {
        let mut orch = LspOrchestrator::new();
        orch.register("rust", "rust-analyzer");
        // Simulate running state
        {
            let handle = orch.servers.get_mut("rust").unwrap();
            handle.status = LspServerStatus::Running;
        }
        orch.stop("rust").unwrap();
        assert_eq!(orch.get("rust").unwrap().status, LspServerStatus::Stopped);
    }

    #[test]
    fn default_is_empty() {
        let orch = LspOrchestrator::default();
        assert!(orch.list().is_empty());
    }

    #[test]
    fn init_request_is_valid_json() {
        let body = init_request_body("rust");
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["method"], "initialize");
        assert_eq!(parsed["params"]["locale"], "rust");
    }
}
