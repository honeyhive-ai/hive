//! `propose_workflow`: the tool form of the `[[workflow: …]]` reply
//! directive, for runtimes that run Hive's tool loop (local models, the
//! OpenAI wire, native Ollama). The directive works on every runtime, but it
//! is only validated after the turn ends; the tool validates inside the turn
//! and hands the error back, so the model can fix its definition and call
//! again.
//!
//! The tool files nothing itself. An accepted payload is kept and, when the
//! reply is final, appended to it as a `[[workflow: …]]` directive
//! ([`append_as_directives`]), so both channels go through the same filing
//! code: a reviewable proposal that becomes a definition only on approval.

use std::sync::Mutex;

use serde_json::{json, Value};

/// The tool name the model calls.
pub const PROPOSE_WORKFLOW: &str = "propose_workflow";

/// Checks a payload against the chat (roster names, existing definitions,
/// DAG rules). `Ok` carries the workflow's name.
pub type Validator = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

pub struct WorkflowTool {
    validate: Validator,
    accepted: Mutex<Vec<String>>,
}

/// Whether a user message asks for a workflow, so the tool is worth a slot
/// this turn. Mirrors the prompt's rule: author one only on explicit request.
pub fn wanted(last_user_message: &str) -> bool {
    let m = last_user_message.to_ascii_lowercase();
    ["workflow", "pipeline", "loop until"].iter().any(|k| m.contains(k))
}

impl WorkflowTool {
    pub fn new(validate: Validator) -> Self {
        Self { validate, accepted: Mutex::new(Vec::new()) }
    }

    /// The tool definition (Anthropic-style `input_schema`; the OpenAI wire
    /// adapter converts it like any other).
    pub fn def() -> Value {
        json!({
            "name": PROPOSE_WORKFLOW,
            "description": "Propose a multi-stage workflow (a DAG of agent stages and human approval gates) \
for this chat. Call this ONLY when the user explicitly asked for a workflow, pipeline or loop. \
The definition is checked now: an error comes back so you can fix it and call again. When accepted \
it is filed for the team to review when your reply ends; it becomes runnable only once a human \
approves it. To change an existing workflow, pass its id (or name) as \"revises\". \
Do not also write a [[workflow:]] directive for the same workflow.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "description": {"type": "string"},
                    "inputLabel": {"type": "string", "description": "What to ask the person for when they start a run"},
                    "revises": {"type": "string", "description": "Id or name of the existing workflow this replaces"},
                    "stages": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string", "description": "Slug, unique in the workflow"},
                                "name": {"type": "string"},
                                "kind": {"type": "string", "enum": ["agent", "gate"]},
                                "agent": {"type": "string", "description": "Roster agent name; omit for the primary runtime"},
                                "prompt": {"type": "string", "description": "Agent stages: may use {{input}} and {{nodes.<upstream id>.output}}"},
                                "after": {"type": "array", "items": {"type": "string"}},
                                "title": {"type": "string", "description": "Gate stages"},
                                "body": {"type": "string", "description": "Gate stages"},
                                "approvals": {"type": "integer", "minimum": 1},
                                "onReject": {"description": "\"halt\" or {\"retryFrom\": \"<upstream stage id>\"}"}
                            },
                            "required": ["id"]
                        }
                    }
                },
                "required": ["name", "stages"]
            }
        })
    }

    /// Validate one call. The reply text is what the model sees.
    pub fn call(&self, input: &Value) -> (String, bool) {
        let json = input.to_string();
        match (self.validate)(&json) {
            Ok(name) => {
                self.accepted.lock().unwrap_or_else(|e| e.into_inner()).push(json);
                (
                    format!(
                        "Accepted: the workflow \"{name}\" will be filed for review when your reply ends. \
A human approves it before it can run. Tell the user it is waiting in the Review pane."
                    ),
                    false,
                )
            }
            Err(e) => (format!("Not filed: {e}. Fix the definition and call {PROPOSE_WORKFLOW} again."), true),
        }
    }

    /// The payloads accepted so far, in call order.
    pub fn accepted(&self) -> Vec<String> {
        self.accepted.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// `body` with each accepted payload appended as a `[[workflow: …]]`
/// directive, skipping any the reply already carries as a directive (a model
/// that both called the tool and wrote the directive files it once).
pub fn append_as_directives(body: &str, accepted: &[String]) -> String {
    if accepted.is_empty() {
        return body.to_string();
    }
    let present: Vec<Value> = crate::directives::parse_reply_directives(body)
        .workflows
        .iter()
        .filter_map(|w| serde_json::from_str(w).ok())
        .collect();
    let mut out = body.to_string();
    for json in accepted {
        let Ok(v) = serde_json::from_str::<Value>(json) else { continue };
        if present.contains(&v) {
            continue;
        }
        out.push_str("\n[[workflow: ");
        out.push_str(json);
        out.push_str("]]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> WorkflowTool {
        WorkflowTool::new(Box::new(|json: &str| {
            let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
            if v["stages"].as_array().is_none_or(|s| s.is_empty()) {
                return Err("a workflow needs at least one stage".into());
            }
            Ok(v["name"].as_str().unwrap_or("").to_string())
        }))
    }

    #[test]
    fn errors_go_back_to_the_model_and_nothing_is_kept() {
        let t = tool();
        let (out, is_error) = t.call(&json!({"name": "X", "stages": []}));
        assert!(is_error);
        assert!(out.contains("at least one stage"), "{out}");
        assert!(t.accepted().is_empty());
    }

    #[test]
    fn accepted_payloads_become_directives_once() {
        let t = tool();
        let input = json!({"name": "Triage", "stages": [{"id": "a", "prompt": "{{input}}"}]});
        let (out, is_error) = t.call(&input);
        assert!(!is_error, "{out}");
        assert!(out.contains("Triage"));
        let body = append_as_directives("Done.", &t.accepted());
        let parsed = crate::directives::parse_reply_directives(&body);
        assert_eq!(parsed.workflows.len(), 1);
        assert_eq!(serde_json::from_str::<Value>(&parsed.workflows[0]).unwrap(), input);

        // The model also wrote the directive itself: not filed twice.
        let both = format!("Done. [[workflow: {input}]]");
        let again = append_as_directives(&both, &t.accepted());
        assert_eq!(crate::directives::parse_reply_directives(&again).workflows.len(), 1);
    }

    #[test]
    fn offered_only_when_the_user_asks_for_one() {
        assert!(wanted("Build me a workflow that triages issues"));
        assert!(wanted("set up a Pipeline for docs"));
        assert!(wanted("draft it and loop until I approve"));
        assert!(!wanted("fix the failing test in calc.py"));
    }
}
