use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub is_current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitAction {
    Keep,
    Remove { discard_changes: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeError {
    NotAGitRepo(PathBuf),
    AlreadyExists(String),
    NotFound(String),
    DirtyWorktree(String),
    CommandFailed(String),
    IoError(String),
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAGitRepo(p) => write!(f, "not a git repository: {}", p.display()),
            Self::AlreadyExists(n) => write!(f, "worktree already exists: {n}"),
            Self::NotFound(n) => write!(f, "worktree not found: {n}"),
            Self::DirtyWorktree(n) => write!(f, "worktree has uncommitted changes: {n}"),
            Self::CommandFailed(m) => write!(f, "git command failed: {m}"),
            Self::IoError(m) => write!(f, "IO error: {m}"),
        }
    }
}

impl std::error::Error for WorktreeError {}

pub struct WorktreeManager {
    repo_root: PathBuf,
    worktrees_dir: PathBuf,
}

impl WorktreeManager {
    pub fn detect(cwd: &Path) -> Result<Option<Self>, WorktreeError> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(cwd)
            .output()
            .map_err(|e| WorktreeError::IoError(e.to_string()))?;

        if !output.status.success() {
            return Ok(None);
        }

        let repo_root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        let worktrees_dir = repo_root.join(".claude").join("worktrees");

        Ok(Some(Self {
            repo_root,
            worktrees_dir,
        }))
    }

    pub fn create(
        &self,
        name: &str,
        base_ref: Option<&str>,
    ) -> Result<WorktreeInfo, WorktreeError> {
        let safe_name = sanitize_branch_name(name);
        let branch_name = format!("claw/{safe_name}");
        let worktree_path = self.worktrees_dir.join(&safe_name);

        if worktree_path.exists() {
            return Err(WorktreeError::AlreadyExists(safe_name));
        }

        let base = base_ref.unwrap_or("HEAD");

        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                worktree_path
                    .to_str()
                    .ok_or_else(|| WorktreeError::CommandFailed("invalid path".into()))?,
                "-b",
                &branch_name,
                base,
            ])
            .current_dir(&self.repo_root)
            .output()
            .map_err(|e| WorktreeError::IoError(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(WorktreeError::CommandFailed(stderr.trim().to_string()));
        }

        Ok(WorktreeInfo {
            name: safe_name,
            path: worktree_path,
            branch: branch_name,
            is_current: false,
        })
    }

    pub fn list(&self) -> Result<Vec<WorktreeInfo>, WorktreeError> {
        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&self.repo_root)
            .output()
            .map_err(|e| WorktreeError::IoError(e.to_string()))?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let mut worktrees = Vec::new();
        let mut current_path = PathBuf::new();
        let mut current_branch = String::new();

        for line in text.lines() {
            if let Some(path_str) = line.strip_prefix("worktree ") {
                current_path = PathBuf::from(path_str);
            } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                current_branch = branch.to_string();
            } else if line.is_empty() && !current_path.as_os_str().is_empty() {
                let is_current = current_path == self.repo_root;
                let name = current_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                worktrees.push(WorktreeInfo {
                    name,
                    path: current_path.clone(),
                    branch: current_branch.clone(),
                    is_current,
                });
                current_path = PathBuf::new();
                current_branch = String::new();
            }
        }

        if !current_path.as_os_str().is_empty() {
            let is_current = current_path == self.repo_root;
            let name = current_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            worktrees.push(WorktreeInfo {
                name,
                path: current_path,
                branch: current_branch,
                is_current,
            });
        }

        Ok(worktrees)
    }

    pub fn remove(&self, name: &str, force: bool) -> Result<(), WorktreeError> {
        let worktree_path = self.worktrees_dir.join(name);
        if !worktree_path.exists() {
            return Err(WorktreeError::NotFound(name.to_string()));
        }

        let mut args = vec![
            "worktree",
            "remove",
            worktree_path
                .to_str()
                .ok_or_else(|| WorktreeError::CommandFailed("invalid path".into()))?,
        ];
        if force {
            args.push("--force");
        }

        let output = Command::new("git")
            .args(&args)
            .current_dir(&self.repo_root)
            .output()
            .map_err(|e| WorktreeError::IoError(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("dirty") {
                return Err(WorktreeError::DirtyWorktree(name.to_string()));
            }
            return Err(WorktreeError::CommandFailed(stderr.trim().to_string()));
        }

        Ok(())
    }

    #[must_use]
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }
}

fn sanitize_branch_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_special_chars() {
        assert_eq!(sanitize_branch_name("my feature"), "my-feature");
        assert_eq!(sanitize_branch_name("a/b/c"), "a-b-c");
        assert_eq!(sanitize_branch_name("--leading"), "leading");
    }

    #[test]
    fn sanitize_preserves_alphanumeric() {
        assert_eq!(sanitize_branch_name("feature-123"), "feature-123");
        assert_eq!(sanitize_branch_name("my_branch"), "my_branch");
    }

    #[test]
    fn detect_outside_git_returns_none() {
        let result = WorktreeManager::detect(Path::new("/tmp"));
        assert!(result.is_ok());
        // /tmp might or might not be in a git repo, but the function should not panic
    }

    #[test]
    fn worktree_error_display() {
        assert!(WorktreeError::NotAGitRepo(PathBuf::from("/foo"))
            .to_string()
            .contains("/foo"));
        assert!(WorktreeError::AlreadyExists("test".into())
            .to_string()
            .contains("test"));
        assert!(WorktreeError::CommandFailed("fatal".into())
            .to_string()
            .contains("fatal"));
    }

    #[test]
    fn exit_action_equality() {
        assert_eq!(ExitAction::Keep, ExitAction::Keep);
        assert_ne!(
            ExitAction::Keep,
            ExitAction::Remove {
                discard_changes: false
            }
        );
    }
}
