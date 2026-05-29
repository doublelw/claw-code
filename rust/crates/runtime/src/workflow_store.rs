use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowSource {
    Project,
    User,
    Builtin,
}

#[derive(Debug, Clone)]
pub struct WorkflowEntry {
    pub name: String,
    pub script: String,
    pub description: Option<String>,
    pub source: WorkflowSource,
}

pub struct WorkflowStore {
    project_dir: PathBuf,
    user_dir: PathBuf,
}

impl WorkflowStore {
    pub fn new(cwd: &Path, config_home: &Path) -> Self {
        Self {
            project_dir: cwd.join(".claude").join("workflows"),
            user_dir: config_home.join("workflows"),
        }
    }

    pub fn discover(&self) -> Vec<WorkflowEntry> {
        let mut entries = BTreeMap::new();

        // Builtin workflows
        let builtins = Self::builtin_workflows();
        for entry in builtins {
            entries.insert(entry.name.clone(), entry);
        }

        // User workflows
        self.load_from_dir(&self.user_dir, WorkflowSource::User, &mut entries);

        // Project workflows (highest priority)
        self.load_from_dir(&self.project_dir, WorkflowSource::Project, &mut entries);

        entries.into_values().collect()
    }

    pub fn load(&self, name: &str) -> Option<WorkflowEntry> {
        // Priority: project > user > builtin
        if let Some(entry) = Self::load_from_path(self, &self.project_dir.join(format!("{name}.js")), name, WorkflowSource::Project) {
            return Some(entry);
        }
        if let Some(entry) = Self::load_from_path(self, &self.user_dir.join(format!("{name}.js")), name, WorkflowSource::User) {
            return Some(entry);
        }
        Self::builtin_workflows().into_iter().find(|e| e.name == name)
    }

    pub fn save_to_project(
        &self,
        name: &str,
        script: &str,
        description: Option<&str>,
    ) -> Result<PathBuf, std::io::Error> {
        self.save_to_dir(&self.project_dir, name, script, description)
    }

    pub fn save_to_user(
        &self,
        name: &str,
        script: &str,
        description: Option<&str>,
    ) -> Result<PathBuf, std::io::Error> {
        self.save_to_dir(&self.user_dir, name, script, description)
    }

    pub fn delete(&self, name: &str, source: WorkflowSource) -> Result<bool, std::io::Error> {
        let dir = match source {
            WorkflowSource::Project => &self.project_dir,
            WorkflowSource::User => &self.user_dir,
            WorkflowSource::Builtin => return Ok(false),
        };
        let path = dir.join(format!("{name}.js"));
        if path.exists() {
            fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn load_from_dir(
        &self,
        dir: &Path,
        source: WorkflowSource,
        entries: &mut BTreeMap<String, WorkflowEntry>,
    ) {
        let Ok(files) = fs::read_dir(dir) else {
            return;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().map(|e| e == "js").unwrap_or(false) {
                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                    if let Some(entry) = Self::load_from_path(self, &path, name, source) {
                        entries.insert(name.to_string(), entry);
                    }
                }
            }
        }
    }

    fn load_from_path(_this: &Self, path: &Path, name: &str, source: WorkflowSource) -> Option<WorkflowEntry> {
        let script = fs::read_to_string(path).ok()?;
        let description = Self::parse_description(&script);
        Some(WorkflowEntry {
            name: name.to_string(),
            script,
            description,
            source,
        })
    }

    fn parse_description(script: &str) -> Option<String> {
        for line in script.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("// @description ") {
                return Some(rest.to_string());
            }
        }
        None
    }

    fn save_to_dir(
        &self,
        dir: &Path,
        name: &str,
        script: &str,
        description: Option<&str>,
    ) -> Result<PathBuf, std::io::Error> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{name}.js"));
        let content = match description {
            Some(desc) => format!("// @description {desc}\n{script}"),
            None => script.to_string(),
        };
        fs::write(&path, content)?;
        Ok(path)
    }

    fn builtin_workflows() -> Vec<WorkflowEntry> {
        let topic = "{{topic}}";
        vec![WorkflowEntry {
            name: "deep-research".to_string(),
            script: crate::workflow_script::WorkflowScript::deep_research_script(topic),
            description: Some("Deep research workflow: multi-angle search, analysis, and synthesis".to_string()),
            source: WorkflowSource::Builtin,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(_name: &str) -> PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    #[test]
    fn discover_empty_when_no_dirs() {
        let store = WorkflowStore::new(Path::new("/nonexistent/project"), Path::new("/nonexistent/home"));
        let entries = store.discover();
        // Should have at least builtin deep-research
        assert!(entries.iter().any(|e| e.name == "deep-research"));
    }

    #[test]
    fn discover_finds_project_workflow() {
        let project_dir = temp_dir("project");
        let home_dir = temp_dir("home");
        let workflows_dir = project_dir.join(".claude").join("workflows");
        fs::create_dir_all(&workflows_dir).unwrap();
        fs::write(workflows_dir.join("audit.js"), "// @description Audit code\nspawnAgent('a','b');").unwrap();

        let store = WorkflowStore::new(&project_dir, &home_dir);
        let entries = store.discover();
        assert!(entries.iter().any(|e| e.name == "audit" && e.source == WorkflowSource::Project));
    }

    #[test]
    fn discover_finds_user_workflow() {
        let project_dir = temp_dir("project");
        let home_dir = temp_dir("home");
        let workflows_dir = home_dir.join("workflows");
        fs::create_dir_all(&workflows_dir).unwrap();
        fs::write(workflows_dir.join("review.js"), "// @description Review\nspawnAgent('a','b');").unwrap();

        let store = WorkflowStore::new(&project_dir, &home_dir);
        let entries = store.discover();
        assert!(entries.iter().any(|e| e.name == "review" && e.source == WorkflowSource::User));
    }

    #[test]
    fn project_overrides_user_same_name() {
        let project_dir = temp_dir("project");
        let home_dir = temp_dir("home");

        let user_wf = home_dir.join("workflows");
        fs::create_dir_all(&user_wf).unwrap();
        fs::write(user_wf.join("test.js"), "user version; spawnAgent('a','b');").unwrap();

        let proj_wf = project_dir.join(".claude").join("workflows");
        fs::create_dir_all(&proj_wf).unwrap();
        fs::write(proj_wf.join("test.js"), "project version; spawnAgent('a','b');").unwrap();

        let store = WorkflowStore::new(&project_dir, &home_dir);
        let entry = store.load("test").unwrap();
        assert_eq!(entry.source, WorkflowSource::Project);
        assert!(entry.script.contains("project version"));
    }

    #[test]
    fn save_creates_directory_and_file() {
        let project_dir = temp_dir("project");
        let home_dir = temp_dir("home");
        let store = WorkflowStore::new(&project_dir, &home_dir);

        let path = store.save_to_project("my-flow", "spawnAgent('a','b');", Some("My flow")).unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("@description My flow"));
    }

    #[test]
    fn load_builtin_by_name() {
        let store = WorkflowStore::new(Path::new("/nonexistent"), Path::new("/nonexistent"));
        let entry = store.load("deep-research").unwrap();
        assert_eq!(entry.source, WorkflowSource::Builtin);
        assert!(entry.script.contains("spawnAgent"));
    }

    #[test]
    fn delete_removes_file() {
        let project_dir = temp_dir("project");
        let home_dir = temp_dir("home");
        let store = WorkflowStore::new(&project_dir, &home_dir);
        store.save_to_user("to-delete", "spawnAgent('a','b');", None).unwrap();
        assert!(store.delete("to-delete", WorkflowSource::User).unwrap());
        assert!(store.load("to-delete").is_none() || store.load("to-delete").map(|e| e.source != WorkflowSource::User).unwrap_or(true));
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let store = WorkflowStore::new(Path::new("/nonexistent"), Path::new("/nonexistent"));
        assert!(!store.delete("nonexistent", WorkflowSource::User).unwrap());
    }

    #[test]
    fn metadata_parsed_from_comment_header() {
        let project_dir = temp_dir("project");
        let home_dir = temp_dir("home");
        let workflows_dir = project_dir.join(".claude").join("workflows");
        fs::create_dir_all(&workflows_dir).unwrap();
        fs::write(
            workflows_dir.join("doc.js"),
            "// @description My documented workflow\nspawnAgent('a','b');",
        )
        .unwrap();

        let store = WorkflowStore::new(&project_dir, &home_dir);
        let entry = store.load("doc").unwrap();
        assert_eq!(entry.description, Some("My documented workflow".to_string()));
    }
}
