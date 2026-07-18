//! Inline agent directives — ported from `parseReactionDirectives` /
//! `finalizeReplyApplyingReactions` in `PrototypeStore.swift`.
//!
//! Agents emit shorthand in their replies:
//! - `[[react: 👍]]` — react to their own message
//! - `[[vote: 👍 👎]]` — prepopulate clickable reaction chips for others
//! - `[[workflow: { …json… }]]` — author a workflow definition (validated and
//!   saved by the app layer; inert until a human runs it)
//!
//! Directives are stripped from the visible body. Pure + unit-tested.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplyDirectives {
    /// Emoji to seed as reactions on the reply, in order, deduped.
    pub emojis: Vec<String>,
    /// Raw JSON payloads of `[[workflow: …]]` directives, in order. Parsing/
    /// validation happens in the app layer (which owns the save path).
    pub workflows: Vec<String>,
    /// The body with the directive markers removed.
    pub cleaned: String,
}

/// Parse `[[react: …]]` / `[[vote: …]]` / `[[workflow: …]]` directives out of
/// an agent reply.
pub fn parse_reply_directives(body: &str) -> ReplyDirectives {
    let mut emojis: Vec<String> = Vec::new();
    let mut workflows: Vec<String> = Vec::new();
    let mut cleaned = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < body.len() {
        if body[i..].starts_with("[[") {
            // Workflow payloads are JSON, which can legally contain "]]"
            // inside strings — scan the balanced object instead of searching
            // for the closer.
            if let Some(rest) = body[i + 2..].strip_prefix("workflow:") {
                let json_start = i + 2 + "workflow:".len() + leading_ws(rest);
                if body[json_start..].starts_with('{') {
                    if let Some(obj_len) = scan_json_object(&body[json_start..]) {
                        let after = json_start + obj_len;
                        let close = after + leading_ws(&body[after..]);
                        if body[close..].starts_with("]]") {
                            workflows.push(body[json_start..json_start + obj_len].to_string());
                            i = close + 2;
                            continue;
                        }
                    }
                }
            }
            if let Some(end_rel) = body[i..].find("]]") {
                let inner = &body[i + 2..i + end_rel];
                let lower = inner.trim_start();
                let payload = lower
                    .strip_prefix("react:")
                    .or_else(|| lower.strip_prefix("vote:"));
                if let Some(payload) = payload {
                    for tok in payload.split_whitespace() {
                        let tok = tok.to_string();
                        if !emojis.contains(&tok) {
                            emojis.push(tok);
                        }
                    }
                    i += end_rel + 2; // skip the whole directive
                    continue;
                }
            }
        }
        // copy one char (respecting UTF-8 boundaries)
        let ch_len = utf8_char_len(bytes[i]);
        cleaned.push_str(&body[i..i + ch_len]);
        i += ch_len;
    }
    ReplyDirectives {
        emojis,
        workflows,
        cleaned: cleaned.trim().to_string(),
    }
}

fn leading_ws(s: &str) -> usize {
    s.len() - s.trim_start().len()
}

/// Byte length of the balanced JSON object starting at `s[0] == '{'`,
/// respecting strings and escapes. `None` if unbalanced.
fn scan_json_object(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (idx, c) in s.bytes().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(idx + 1);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_react_and_vote_and_strips() {
        let r = parse_reply_directives("Shipping it. [[vote: 👍 👎]] thoughts? [[react: 🎉]]");
        assert_eq!(r.emojis, vec!["👍", "👎", "🎉"]);
        assert_eq!(r.cleaned, "Shipping it.  thoughts?");
    }

    #[test]
    fn no_directives_passes_through() {
        let r = parse_reply_directives("just a normal reply");
        assert!(r.emojis.is_empty());
        assert!(r.workflows.is_empty());
        assert_eq!(r.cleaned, "just a normal reply");
    }

    #[test]
    fn dedups_repeated_emoji() {
        let r = parse_reply_directives("[[vote: 👍 👍 👎]]");
        assert_eq!(r.emojis, vec!["👍", "👎"]);
    }

    #[test]
    fn extracts_workflow_json_and_strips() {
        let body = "Here you go.\n[[workflow: {\"name\": \"Triage\", \"stages\": [{\"id\": \"scan\", \"prompt\": \"{{input}}\"}]}]]\nRun it from the pane.";
        let r = parse_reply_directives(body);
        assert_eq!(r.workflows.len(), 1);
        assert!(r.workflows[0].starts_with("{\"name\""));
        assert_eq!(r.cleaned, "Here you go.\n\nRun it from the pane.");
        // The payload parses as the JSON that was embedded.
        let v: serde_json::Value = serde_json::from_str(&r.workflows[0]).unwrap();
        assert_eq!(v["name"], "Triage");
    }

    #[test]
    fn workflow_json_may_contain_double_brackets_in_strings() {
        let body = r#"[[workflow: {"name": "x", "note": "arrays like [[1]] are fine", "stages": []}]]"#;
        let r = parse_reply_directives(body);
        assert_eq!(r.workflows.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&r.workflows[0]).unwrap();
        assert_eq!(v["note"], "arrays like [[1]] are fine");
        assert_eq!(r.cleaned, "");
    }

    #[test]
    fn malformed_workflow_directive_is_left_verbatim() {
        // Unbalanced JSON → not treated as a directive; text passes through so
        // the user can see what the agent attempted.
        let body = "[[workflow: {\"name\": \"broken\"]]";
        let r = parse_reply_directives(body);
        assert!(r.workflows.is_empty());
        assert_eq!(r.cleaned, body);
    }

    #[test]
    fn workflow_and_reactions_coexist() {
        let body = "Done. [[react: ✅]] [[workflow: {\"name\": \"w\", \"stages\": []}]]";
        let r = parse_reply_directives(body);
        assert_eq!(r.emojis, vec!["✅"]);
        assert_eq!(r.workflows.len(), 1);
        assert_eq!(r.cleaned, "Done.");
    }
}
