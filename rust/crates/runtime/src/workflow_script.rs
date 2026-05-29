use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptValidationError {
    pub errors: Vec<String>,
}

impl fmt::Display for ScriptValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.errors.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "script validation error: {e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ScriptValidationError {}

pub struct WorkflowScript;

impl WorkflowScript {
    pub fn validate(script: &str) -> Result<(), ScriptValidationError> {
        let mut errors = Vec::new();

        if script.trim().is_empty() {
            errors.push("script is empty".to_string());
            return Err(ScriptValidationError { errors });
        }

        if script.len() > 100_000 {
            errors.push(format!(
                "script exceeds 100KB limit ({} bytes)",
                script.len()
            ));
        }

        let forbidden = [
            ("import ", "import statements are not allowed"),
            ("require(", "require() calls are not allowed"),
            ("fetch(", "fetch() calls are not allowed"),
            ("eval(", "eval() calls are not allowed"),
            ("new Function(", "new Function() is not allowed"),
        ];

        for (pattern, msg) in &forbidden {
            if script.contains(pattern) {
                errors.push((*msg).to_string());
            }
        }

        if !script.contains("spawnAgent") {
            errors.push("script must contain at least one spawnAgent() call".to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ScriptValidationError { errors })
        }
    }

    pub fn auto_generation_prompt(task: &str) -> String {
        format!(
            r#"You are generating a workflow script for the following task:
{task}

The script has access to these globals:
- spawnAgent(description: string, prompt: string): string — spawns a subagent, returns its ID
- waitForAgent(agentId: string): string — waits for an agent to finish, returns its output
- log(message: string): void — appends to the workflow output
- getEnv(key: string): string | null — reads an environment variable
- JSON, Math, console — standard JS builtins

Constraints:
- Max 16 concurrent agents
- Max 1000 total agents per run
- No import, require, fetch, eval, or new Function
- Use log() for progress reporting
- Handle errors with try/catch

Example:
```js
const agents = [];
for (let i = 0; i < 5; i++) {{
  const id = spawnAgent("researcher", `Research aspect ${{i+1}} of: {task}`);
  agents.push(id);
}}
const results = agents.map(id => waitForAgent(id));
log("All research complete: " + results.length + " results");
```

Generate a workflow script that accomplishes the task efficiently using parallel agents."#
        )
    }

    pub fn fan_out_template(count: usize, task_template: &str) -> String {
        let mut script = String::from("const agents = [];\n");
        for i in 0..count {
            script.push_str(&format!(
                "agents.push(spawnAgent(\"worker-{}\", `{task_template} (part {})`));\n",
                i + 1,
                i + 1
            ));
        }
        script.push_str("const results = agents.map(id => waitForAgent(id));\n");
        script.push_str(&format!(
            "log(\"Fan-out complete: {} results collected\");\n",
            count
        ));
        script
    }

    pub fn pipeline_template(steps: &[&str]) -> String {
        let mut script = String::from("let result = '';\n");
        for (i, step) in steps.iter().enumerate() {
            script.push_str(&format!(
                "const agent{i} = spawnAgent(\"pipeline-step-{i}\", `{step}` + \"\\nPrevious output: \" + result);\n"
            ));
            script.push_str(&format!("result = waitForAgent(agent{i});\n"));
        }
        script.push_str("log(\"Pipeline complete: \" + result);\n");
        script
    }

    pub fn adversarial_review_template(task: &str) -> String {
        format!(
            r#"const implementer = spawnAgent("implementer", "Implement the following: {task}");
const impl_result = waitForAgent(implementer);
log("Implementation complete");

const reviewer = spawnAgent("reviewer", "Review this output for bugs, style, and correctness. Be critical:\\n" + impl_result);
const review = waitForAgent(reviewer);
log("Review complete");

const reviser = spawnAgent("reviser", "Revise the following based on this review feedback.\\n\\nOriginal:\\n" + impl_result + "\\n\\nReview:\\n" + review);
const final_result = waitForAgent(reviser);
log("Revision complete");
final_result"#
        )
    }

    pub fn voting_template(count: usize, task: &str) -> String {
        let mut agents = String::new();
        for i in 0..count {
            agents.push_str(&format!(
                "agents.push(spawnAgent(\"voter-{i}\", `Solve independently: {task}`));\n"
            ));
        }
        format!(
            r#"const agents = [];
{agents}const results = agents.map(id => waitForAgent(id));
log("All {} votes collected");

const judge = spawnAgent("judge", "Evaluate these {} solutions and pick the best one. Explain your choice.\\n\\n" + results.join("\\n\\n---\\n\\n"));
const winner = waitForAgent(judge);
log("Winner selected");
winner"#,
            count, count
        )
    }

    pub fn deep_research_script(topic: &str) -> String {
        format!(
            r#"// Deep Research Workflow: {topic}
// Phase 1: Parallel search across different angles
const search_agents = [];
for (let i = 0; i < 5; i++) {{
  const angles = ["overview", "technical details", "recent developments", "criticisms", "practical applications"];
  search_agents.push(spawnAgent("search-" + i, `Research the {topic} from the angle of: ${{angles[i]}}. Use WebSearch to find relevant sources. Summarize key findings with citations.`));
}}
const search_results = search_agents.map(id => waitForAgent(id));
log("Phase 1: Search complete (" + search_results.length + " angles covered)");

// Phase 2: Analysis of search results
const analysis_agents = [];
for (let i = 0; i < 3; i++) {{
  const start = Math.floor(i * search_results.length / 3);
  const end = Math.floor((i + 1) * search_results.length / 3);
  const subset = search_results.slice(start, end).join("\\n---\\n");
  analysis_agents.push(spawnAgent("analysis-" + i, `Analyze these research findings for key claims, supporting evidence, and contradictions:\\n${{subset}}`));
}}
const analysis_results = analysis_agents.map(id => waitForAgent(id));
log("Phase 2: Analysis complete");

// Phase 3: Synthesis
const synthesizer = spawnAgent("synthesizer", `Synthesize the following analyses into a coherent research report on: {topic}. Include citations. Filter out claims that lack supporting evidence.\\n\\n` + analysis_results.join("\\n\\n"));
const synthesis = waitForAgent(synthesizer);
log("Phase 3: Synthesis complete");

// Phase 4: Review
const reviewer = spawnAgent("reviewer", `Review this research report for accuracy, completeness, and citation quality. Flag any unsupported claims:\\n${{synthesis}}`);
const review = waitForAgent(reviewer);
log("Phase 4: Review complete");

// Phase 5: Final revision
const reviser = spawnAgent("reviser", `Produce the final research report incorporating this review feedback:\\n\\nReport:\\n${{synthesis}}\\n\\nReview:\\n${{review}}`);
const final_report = waitForAgent(reviser);
log("Phase 5: Final revision complete");

final_report"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_fan_out_script() {
        let script = "const id = spawnAgent(\"w\", \"do work\"); waitForAgent(id);";
        assert!(WorkflowScript::validate(script).is_ok());
    }

    #[test]
    fn rejects_empty_script() {
        let err = WorkflowScript::validate("").unwrap_err();
        assert!(err.errors[0].contains("empty"));
    }

    #[test]
    fn rejects_whitespace_only_script() {
        let err = WorkflowScript::validate("   \n\t  ").unwrap_err();
        assert!(err.errors[0].contains("empty"));
    }

    #[test]
    fn rejects_import_statement() {
        let script = "import fs from 'fs'; spawnAgent('a','b');";
        let err = WorkflowScript::validate(script).unwrap_err();
        assert!(err.errors.iter().any(|e| e.contains("import")));
    }

    #[test]
    fn rejects_require_call() {
        let script = "const x = require('fs'); spawnAgent('a','b');";
        let err = WorkflowScript::validate(script).unwrap_err();
        assert!(err.errors.iter().any(|e| e.contains("require")));
    }

    #[test]
    fn rejects_fetch_call() {
        let script = "fetch('http://evil.com'); spawnAgent('a','b');";
        let err = WorkflowScript::validate(script).unwrap_err();
        assert!(err.errors.iter().any(|e| e.contains("fetch")));
    }

    #[test]
    fn rejects_eval_call() {
        let script = "eval('malicious'); spawnAgent('a','b');";
        let err = WorkflowScript::validate(script).unwrap_err();
        assert!(err.errors.iter().any(|e| e.contains("eval")));
    }

    #[test]
    fn rejects_new_function() {
        let script = "new Function('return 1')(); spawnAgent('a','b');";
        let err = WorkflowScript::validate(script).unwrap_err();
        assert!(err.errors.iter().any(|e| e.contains("new Function")));
    }

    #[test]
    fn rejects_script_without_spawn_agent() {
        let script = "log('hello');";
        let err = WorkflowScript::validate(script).unwrap_err();
        assert!(err.errors.iter().any(|e| e.contains("spawnAgent")));
    }

    #[test]
    fn fan_out_template_passes_validation() {
        let script = WorkflowScript::fan_out_template(5, "research topic");
        assert!(WorkflowScript::validate(&script).is_ok());
    }

    #[test]
    fn pipeline_template_passes_validation() {
        let script = WorkflowScript::pipeline_template(&["step 1", "step 2", "step 3"]);
        assert!(WorkflowScript::validate(&script).is_ok());
    }

    #[test]
    fn adversarial_review_template_passes_validation() {
        let script = WorkflowScript::adversarial_review_template("implement auth module");
        assert!(WorkflowScript::validate(&script).is_ok());
    }

    #[test]
    fn voting_template_passes_validation() {
        let script = WorkflowScript::voting_template(3, "solve the problem");
        assert!(WorkflowScript::validate(&script).is_ok());
    }

    #[test]
    fn auto_generation_prompt_mentions_spawn_agent() {
        let prompt = WorkflowScript::auto_generation_prompt("test task");
        assert!(prompt.contains("spawnAgent"));
        assert!(prompt.contains("test task"));
    }

    #[test]
    fn auto_generation_prompt_includes_concurrency_limits() {
        let prompt = WorkflowScript::auto_generation_prompt("test");
        assert!(prompt.contains("16 concurrent"));
        assert!(prompt.contains("1000 total"));
    }

    #[test]
    fn deep_research_script_passes_validation() {
        let script = WorkflowScript::deep_research_script("quantum computing");
        assert!(WorkflowScript::validate(&script).is_ok());
    }

    #[test]
    fn deep_research_script_contains_phases() {
        let script = WorkflowScript::deep_research_script("test topic");
        assert!(script.contains("Phase 1"));
        assert!(script.contains("Phase 5"));
        assert!(script.contains("test topic"));
    }

    #[test]
    fn deep_research_script_uses_provided_topic() {
        let script = WorkflowScript::deep_research_script("machine learning");
        assert!(script.contains("machine learning"));
    }
}
