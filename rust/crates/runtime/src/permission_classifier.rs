use std::path::Path;

fn extract_json_field(input: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{}\":", field);
    let start = input.find(&pattern)?;
    let rest = &input[start + pattern.len()..];
    let rest = rest.trim_start();
    if rest.starts_with('"') {
        let end = rest[1..].find('"')?;
        Some(rest[1..end + 1].to_string())
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    Allow,
    Deny,
    Prompt,
}

pub struct PermissionClassifier {
    workspace_root: Option<String>,
}

impl PermissionClassifier {
    pub fn new(workspace_root: Option<&Path>) -> Self {
        Self {
            workspace_root: workspace_root.map(|p| p.to_string_lossy().to_string()),
        }
    }

    pub fn classify(&self, tool_name: &str, input: &str) -> Classification {
        match tool_name {
            "bash" | "PowerShell" => {
                let command = extract_json_field(input, "command").unwrap_or_else(|| input.to_string());
                self.classify_bash(&command)
            }
            "read_file" | "glob_search" | "grep_search" => Classification::Allow,
            "write_file" | "edit_file" => self.classify_file_write(input),
            "GitStatus" | "GitLog" | "GitDiff" | "GitShow" | "GitBlame" => Classification::Allow,
            "WebFetch" | "WebSearch" => Classification::Prompt,
            "TaskCreate" | "TaskGet" | "TaskList" | "TaskOutput" => Classification::Allow,
            "TaskStop" | "TaskUpdate" | "RunTaskPacket" => Classification::Prompt,
            "Agent" | "Skill" => Classification::Allow,
            "MCP" => Classification::Allow,
            "NotebookEdit" => self.classify_file_write(input),
            _ => Classification::Prompt,
        }
    }

    fn classify_bash(&self, command: &str) -> Classification {
        let lower = command.to_ascii_lowercase();
        let trimmed = lower.trim();

        let read_only_prefixes = [
            "cat ", "head ", "tail ", "less ", "more ", "ls", "ll", "dir ",
            "find ", "test ", "grep ", "rg ", "rg", "awk ", "sed -n", "file ",
            "stat ", "readlink ", "wc ", "sort ", "uniq ", "cut ", "tr ",
            "pwd", "echo ", "printf ", "git status", "git log", "git diff",
            "git show", "git blame", "git branch", "git remote", "git tag",
            "which ", "type ", "env", "printenv", "node --version", "cargo --version",
            "rustc --version", "python3 --version", "python --version",
        ];

        for prefix in &read_only_prefixes {
            if trimmed.starts_with(prefix) {
                return Classification::Allow;
            }
        }

        let destructive_patterns = [
            "rm ", "rm -", "rmdir", "del ", "format ", "mkfs",
            "dd ", "> /dev/", "shutdown", "reboot", "halt",
            "chmod 777", "chown root",
        ];

        for pattern in &destructive_patterns {
            if trimmed.contains(pattern) {
                return Classification::Deny;
            }
        }

        Classification::Prompt
    }

    fn classify_file_write(&self, input: &str) -> Classification {
        let sensitive_patterns = [
            ".env", "credentials", "secret", "password", "token",
            ".pem", ".key", "id_rsa", "id_ed25519", ".ssh",
        ];

        let lower = input.to_ascii_lowercase();
        for pattern in &sensitive_patterns {
            if lower.contains(pattern) {
                return Classification::Deny;
            }
        }

        if self.workspace_root.is_some() {
            if lower.contains("../") || lower.contains("..\\") {
                return Classification::Deny;
            }
        }

        Classification::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> PermissionClassifier {
        PermissionClassifier::new(Some(Path::new("/workspace")))
    }

    #[test]
    fn bash_read_only_cat() {
        assert_eq!(classifier().classify("bash", r#"{"command":"cat README.md"}"#), Classification::Allow);
    }

    #[test]
    fn bash_read_only_git_status() {
        assert_eq!(classifier().classify("bash", r#"{"command":"git status"}"#), Classification::Allow);
    }

    #[test]
    fn bash_read_only_ls() {
        assert_eq!(classifier().classify("bash", r#"{"command":"ls -la"}"#), Classification::Allow);
    }

    #[test]
    fn bash_read_only_grep() {
        assert_eq!(classifier().classify("bash", r#"{"command":"grep -r pattern src/"}"#), Classification::Allow);
    }

    #[test]
    fn bash_destructive_rm() {
        assert_eq!(classifier().classify("bash", r#"{"command":"rm -rf /"}"#), Classification::Deny);
    }

    #[test]
    fn bash_destructive_format() {
        assert_eq!(classifier().classify("bash", r#"{"command":"format c:""#), Classification::Deny);
    }

    #[test]
    fn bash_unknown_prompts() {
        assert_eq!(classifier().classify("bash", r#"{"command":"cargo build"}"#), Classification::Prompt);
    }

    #[test]
    fn read_tools_auto_allow() {
        assert_eq!(classifier().classify("read_file", r#"{"path":"src/lib.rs"}"#), Classification::Allow);
        assert_eq!(classifier().classify("glob_search", r#"{"pattern":"*.rs"}"#), Classification::Allow);
        assert_eq!(classifier().classify("grep_search", r#"{"pattern":"TODO"}"#), Classification::Allow);
    }

    #[test]
    fn write_inside_workspace_allow() {
        assert_eq!(classifier().classify("write_file", r#"{"path":"src/new.rs","content":"fn main(){}"}"#), Classification::Allow);
    }

    #[test]
    fn write_env_deny() {
        assert_eq!(classifier().classify("write_file", r#"{"path":".env","content":"KEY=VAL"}"#), Classification::Deny);
    }

    #[test]
    fn write_credentials_deny() {
        assert_eq!(classifier().classify("write_file", r#"{"path":"credentials.json"}"#), Classification::Deny);
    }

    #[test]
    fn write_traversal_deny() {
        assert_eq!(classifier().classify("write_file", r#"{"path":"../../etc/passwd"}"#), Classification::Deny);
    }

    #[test]
    fn git_read_tools_allow() {
        assert_eq!(classifier().classify("GitStatus", "{}"), Classification::Allow);
        assert_eq!(classifier().classify("GitLog", "{}"), Classification::Allow);
        assert_eq!(classifier().classify("GitDiff", "{}"), Classification::Allow);
    }

    #[test]
    fn web_tools_prompt() {
        assert_eq!(classifier().classify("WebFetch", r#"{"url":"http://example.com"}"#), Classification::Prompt);
        assert_eq!(classifier().classify("WebSearch", r#"{"query":"test"}"#), Classification::Prompt);
    }

    #[test]
    fn task_read_allow() {
        assert_eq!(classifier().classify("TaskGet", r#"{"task_id":"t1"}"#), Classification::Allow);
        assert_eq!(classifier().classify("TaskList", "{}"), Classification::Allow);
    }

    #[test]
    fn task_stop_prompt() {
        assert_eq!(classifier().classify("TaskStop", r#"{"task_id":"t1"}"#), Classification::Prompt);
    }

    #[test]
    fn unknown_tool_prompts() {
        assert_eq!(classifier().classify("UnknownTool", "{}"), Classification::Prompt);
    }
}
