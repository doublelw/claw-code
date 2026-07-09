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
    /// v2.1.183: when true, destructive VCS/IaC commands (git reset --hard,
    /// terraform destroy, etc.) bypass the safety block. Set when the user
    /// explicitly asked to discard local work or destroy a stack.
    allow_destructive_vcs: bool,
}

impl PermissionClassifier {
    pub fn new(workspace_root: Option<&Path>) -> Self {
        Self {
            workspace_root: workspace_root.map(|p| p.to_string_lossy().to_string()),
            allow_destructive_vcs: false,
        }
    }

    /// v2.1.183: opt in to destructive VCS/IaC commands for this classifier.
    #[must_use]
    pub fn with_destructive_vcs_allowed(mut self, allowed: bool) -> Self {
        self.allow_destructive_vcs = allowed;
        self
    }

    pub fn classify(&self, tool_name: &str, input: &str) -> Classification {
        match tool_name {
            "bash" | "PowerShell" => {
                let command =
                    extract_json_field(input, "command").unwrap_or_else(|| input.to_string());
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

        // v2.1.205: block any command that targets a session transcript file,
        // checked first so read-only-looking prefixes (e.g. `echo x > ...`) can't
        // smuggle a transcript rewrite past the guard.
        if Self::targets_session_transcript(trimmed) {
            return Classification::Deny;
        }

        let read_only_prefixes = [
            "cat ",
            "head ",
            "tail ",
            "less ",
            "more ",
            "ls",
            "ll",
            "dir ",
            "find ",
            "test ",
            "grep ",
            "rg ",
            "rg",
            "awk ",
            "sed -n",
            "file ",
            "stat ",
            "readlink ",
            "wc ",
            "sort ",
            "uniq ",
            "cut ",
            "tr ",
            "pwd",
            "echo ",
            "printf ",
            "git status",
            "git log",
            "git diff",
            "git show",
            "git blame",
            "git branch",
            "git remote",
            "git tag",
            "which ",
            "type ",
            "env",
            "printenv",
            "node --version",
            "cargo --version",
            "rustc --version",
            "python3 --version",
            "python --version",
        ];

        for prefix in &read_only_prefixes {
            if trimmed.starts_with(prefix) {
                return Classification::Allow;
            }
        }

        let destructive_patterns = [
            "rm ",
            "rm -",
            "rmdir",
            "del ",
            "format ",
            "mkfs",
            "dd ",
            "> /dev/",
            "shutdown",
            "reboot",
            "halt",
            "chmod 777",
            "chown root",
        ];

        for pattern in &destructive_patterns {
            if trimmed.contains(pattern) {
                return Classification::Deny;
            }
        }

        // v2.1.183: destructive version-control / IaC commands are blocked by
        // default. The agent must have been explicitly asked to discard local
        // work, amend a session-made commit, or destroy a specific stack.
        // These surface as Deny unless the caller opts in via the
        // `allow_destructive_vcs` escape hatch.
        if !self.allow_destructive_vcs && Self::is_destructive_vcs_command(trimmed) {
            return Classification::Deny;
        }

        Classification::Prompt
    }

    /// v2.1.183: detect commands that discard local work or destroy
    /// infrastructure. These match Claude Code's auto-mode safety list.
    fn is_destructive_vcs_command(trimmed: &str) -> bool {
        let destructive_vcs = [
            // git: discard uncommitted work
            "git reset --hard",
            "git checkout -- .",
            "git checkout --",
            "git clean -fd",
            "git clean -fdx",
            "git stash drop",
            // git: rewrite history not made by the agent this session
            "git commit --amend",
            // IaC destroyers (must name the specific stack to be allowed)
            "terraform destroy",
            "pulumi destroy",
            "cdk destroy",
        ];
        destructive_vcs.iter().any(|p| trimmed.contains(p))
    }

    fn classify_file_write(&self, input: &str) -> Classification {
        let sensitive_patterns = [
            ".env",
            "credentials",
            "secret",
            "password",
            "token",
            ".pem",
            ".key",
            "id_rsa",
            "id_ed25519",
            ".ssh",
        ];

        let lower = input.to_ascii_lowercase();
        for pattern in &sensitive_patterns {
            if lower.contains(pattern) {
                return Classification::Deny;
            }
        }

        // v2.1.205: block tampering with session transcript files. The agent
        // must never rewrite its own conversation log — that would let it
        // rewrite history, hide tool calls, or corrupt the audit trail.
        if Self::targets_session_transcript(&lower) {
            return Classification::Deny;
        }

        if self.workspace_root.is_some() {
            if lower.contains("../") || lower.contains("..\\") {
                return Classification::Deny;
            }
        }

        Classification::Allow
    }

    /// v2.1.205: detect paths/inputs targeting a session transcript file.
    /// claw-code stores transcripts under `.claw/sessions/<id>/session-*.jsonl`
    /// (and the Claude-compatible `.claude/sessions/...`). Any write/edit to a
    /// `session-*.jsonl` inside a `sessions/` directory is treated as tampering.
    fn targets_session_transcript(lower: &str) -> bool {
        (lower.contains("/sessions/") || lower.contains("\\sessions\\"))
            && lower.contains("session-")
            && lower.contains(".jsonl")
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
        assert_eq!(
            classifier().classify("bash", r#"{"command":"cat README.md"}"#),
            Classification::Allow
        );
    }

    #[test]
    fn bash_read_only_git_status() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"git status"}"#),
            Classification::Allow
        );
    }

    #[test]
    fn bash_read_only_ls() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"ls -la"}"#),
            Classification::Allow
        );
    }

    #[test]
    fn bash_read_only_grep() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"grep -r pattern src/"}"#),
            Classification::Allow
        );
    }

    #[test]
    fn bash_destructive_rm() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"rm -rf /"}"#),
            Classification::Deny
        );
    }

    #[test]
    fn bash_destructive_format() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"format c:""#),
            Classification::Deny
        );
    }

    #[test]
    fn bash_unknown_prompts() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"cargo build"}"#),
            Classification::Prompt
        );
    }

    #[test]
    fn read_tools_auto_allow() {
        assert_eq!(
            classifier().classify("read_file", r#"{"path":"src/lib.rs"}"#),
            Classification::Allow
        );
        assert_eq!(
            classifier().classify("glob_search", r#"{"pattern":"*.rs"}"#),
            Classification::Allow
        );
        assert_eq!(
            classifier().classify("grep_search", r#"{"pattern":"TODO"}"#),
            Classification::Allow
        );
    }

    #[test]
    fn write_inside_workspace_allow() {
        assert_eq!(
            classifier().classify(
                "write_file",
                r#"{"path":"src/new.rs","content":"fn main(){}"}"#
            ),
            Classification::Allow
        );
    }

    #[test]
    fn write_env_deny() {
        assert_eq!(
            classifier().classify("write_file", r#"{"path":".env","content":"KEY=VAL"}"#),
            Classification::Deny
        );
    }

    #[test]
    fn write_credentials_deny() {
        assert_eq!(
            classifier().classify("write_file", r#"{"path":"credentials.json"}"#),
            Classification::Deny
        );
    }

    #[test]
    fn write_traversal_deny() {
        assert_eq!(
            classifier().classify("write_file", r#"{"path":"../../etc/passwd"}"#),
            Classification::Deny
        );
    }

    #[test]
    fn git_read_tools_allow() {
        assert_eq!(
            classifier().classify("GitStatus", "{}"),
            Classification::Allow
        );
        assert_eq!(classifier().classify("GitLog", "{}"), Classification::Allow);
        assert_eq!(
            classifier().classify("GitDiff", "{}"),
            Classification::Allow
        );
    }

    #[test]
    fn web_tools_prompt() {
        assert_eq!(
            classifier().classify("WebFetch", r#"{"url":"http://example.com"}"#),
            Classification::Prompt
        );
        assert_eq!(
            classifier().classify("WebSearch", r#"{"query":"test"}"#),
            Classification::Prompt
        );
    }

    #[test]
    fn task_read_allow() {
        assert_eq!(
            classifier().classify("TaskGet", r#"{"task_id":"t1"}"#),
            Classification::Allow
        );
        assert_eq!(
            classifier().classify("TaskList", "{}"),
            Classification::Allow
        );
    }

    #[test]
    fn task_stop_prompt() {
        assert_eq!(
            classifier().classify("TaskStop", r#"{"task_id":"t1"}"#),
            Classification::Prompt
        );
    }

    #[test]
    fn unknown_tool_prompts() {
        assert_eq!(
            classifier().classify("UnknownTool", "{}"),
            Classification::Prompt
        );
    }

    // --- v2.1.183: destructive VCS / IaC safety ---

    #[test]
    fn bash_destructive_git_reset_hard_blocked() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"git reset --hard HEAD~1"}"#),
            Classification::Deny
        );
    }

    #[test]
    fn bash_destructive_git_clean_blocked() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"git clean -fd"}"#),
            Classification::Deny
        );
    }

    #[test]
    fn bash_destructive_git_commit_amend_blocked() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"git commit --amend --no-edit"}"#),
            Classification::Deny
        );
    }

    #[test]
    fn bash_destructive_terraform_destroy_blocked() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"terraform destroy -auto-approve"}"#),
            Classification::Deny
        );
    }

    #[test]
    fn bash_destructive_pulumi_cdk_destroy_blocked() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"pulumi destroy --yes"}"#),
            Classification::Deny
        );
        assert_eq!(
            classifier().classify("bash", r#"{"command":"cdk destroy --force"}"#),
            Classification::Deny
        );
    }

    #[test]
    fn bash_destructive_git_stash_drop_blocked() {
        assert_eq!(
            classifier().classify("bash", r#"{"command":"git stash drop stash@{0}"}"#),
            Classification::Deny
        );
    }

    #[test]
    fn bash_destructive_vcs_allowed_when_opted_in() {
        let permissive = classifier().with_destructive_vcs_allowed(true);
        assert_eq!(
            permissive.classify("bash", r#"{"command":"git reset --hard"}"#),
            Classification::Prompt
        );
    }

    #[test]
    fn bash_non_destructive_git_not_blocked() {
        // Regular git operations still allowed/prompted, not caught by the
        // destructive VCS guard.
        assert_eq!(
            classifier().classify("bash", r#"{"command":"git reset"}"#),
            Classification::Prompt
        );
        assert_eq!(
            classifier().classify("bash", r#"{"command":"git commit -m msg"}"#),
            Classification::Prompt
        );
    }

    // --- v2.1.205: session transcript tampering blocker ---

    #[test]
    fn write_to_session_transcript_blocked() {
        assert_eq!(
            classifier().classify(
                "write_file",
                r#"{"path":".claw/sessions/abc123/session-1234.jsonl","content":"x"}"#,
            ),
            Classification::Deny
        );
    }

    #[test]
    fn edit_to_session_transcript_blocked() {
        assert_eq!(
            classifier().classify(
                "edit_file",
                r#"{"path":".claude/sessions/s1/session-9.jsonl"}"#,
            ),
            Classification::Deny
        );
    }

    #[test]
    fn bash_writing_to_transcript_blocked() {
        assert_eq!(
            classifier().classify(
                "bash",
                r#"{"command":"echo x > .claw/sessions/abc/session-1.jsonl"}"#,
            ),
            Classification::Deny
        );
    }

    #[test]
    fn non_transcript_jsonl_allowed() {
        // A random .jsonl data file is not a session transcript.
        assert_eq!(
            classifier().classify(
                "write_file",
                r#"{"path":"data/exports/log.jsonl","content":"{}"}"#,
            ),
            Classification::Allow
        );
    }

    #[test]
    fn normal_workspace_write_still_allowed() {
        assert_eq!(
            classifier().classify(
                "write_file",
                r#"{"path":"src/main.rs","content":"fn main(){}"}"#,
            ),
            Classification::Allow
        );
    }
}
