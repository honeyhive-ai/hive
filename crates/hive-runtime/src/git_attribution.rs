//! Git commit attribution for host-side agent execution.
//!
//! When the host runs an agent turn that a teammate drove, commits should be
//! credited to that teammate — not the host. Git's author/committer split makes
//! this clean: we set `GIT_AUTHOR_*` to the **requester** (the person whose
//! message triggered the turn) and leave the committer as the host, and we
//! surface `Co-authored-by:` trailers for every *other* human who took part in
//! the exchange (so 3+ person threads credit everyone).
//!
//! `GIT_AUTHOR_*` is honored by any `git commit` the agent shells out to; the
//! co-author trailers are injected into the system prompt for the agent to add
//! to its commit message (git has no env for trailers).

use hive_core::ActorIdentity;
use std::collections::HashSet;

/// A stable, valid-looking email for an actor — their configured git email, or
/// a deterministic noreply fallback so attribution still works unconfigured.
pub fn email_for(actor: &ActorIdentity) -> String {
    if let Some(e) = actor.git_email.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return e.to_string();
    }
    let slug: String = actor
        .display_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "user" } else { slug };
    format!("{slug}@users.noreply.hive.local")
}

/// `GIT_AUTHOR_*` env so the requester is the commit author. The committer is
/// intentionally left to the host's git config.
pub fn author_env(requester: &ActorIdentity) -> Vec<(String, String)> {
    vec![
        ("GIT_AUTHOR_NAME".to_string(), requester.display_name.clone()),
        ("GIT_AUTHOR_EMAIL".to_string(), email_for(requester)),
    ]
}

/// `Co-authored-by:` trailers for every human participant other than the
/// author (deduped by actor id; agents/assistants excluded by the caller).
pub fn coauthor_trailers(author: &ActorIdentity, participants: &[ActorIdentity]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    seen.insert(author.id.as_str());
    let mut out = Vec::new();
    for p in participants {
        if seen.insert(p.id.as_str()) {
            out.push(format!("Co-authored-by: {} <{}>", p.display_name, email_for(p)));
        }
    }
    out
}

/// A system-prompt fragment instructing the agent to credit collaborators when
/// it commits. Empty when there are no co-authors.
pub fn commit_attribution_note(author: &ActorIdentity, participants: &[ActorIdentity]) -> String {
    let trailers = coauthor_trailers(author, participants);
    if trailers.is_empty() {
        return String::new();
    }
    format!(
        "When you create a git commit, attribute collaborators by appending these \
         trailers (after a blank line) to the commit message:\n{}",
        trailers.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::ActorKind;

    fn actor(id: &str, name: &str, email: Option<&str>) -> ActorIdentity {
        let mut a = ActorIdentity::new(id, name, ActorKind::Human);
        a.git_email = email.map(|e| e.to_string());
        a
    }

    #[test]
    fn uses_configured_email_else_noreply_fallback() {
        assert_eq!(email_for(&actor("1", "Sam", Some("sam@x.com"))), "sam@x.com");
        assert_eq!(email_for(&actor("2", "Mara Lee", None)), "mara-lee@users.noreply.hive.local");
        assert_eq!(email_for(&actor("3", "  ", None)), "user@users.noreply.hive.local");
    }

    #[test]
    fn author_env_sets_git_author() {
        let env = author_env(&actor("1", "Sam", Some("sam@x.com")));
        assert!(env.contains(&("GIT_AUTHOR_NAME".into(), "Sam".into())));
        assert!(env.contains(&("GIT_AUTHOR_EMAIL".into(), "sam@x.com".into())));
        // Committer is NOT overridden — stays the host's identity.
        assert!(!env.iter().any(|(k, _)| k == "GIT_COMMITTER_NAME"));
    }

    #[test]
    fn coauthors_exclude_the_author_and_dedupe() {
        let sam = actor("1", "Sam", Some("sam@x.com"));
        let alex = actor("2", "Alex", Some("alex@y.com"));
        let alex_dup = actor("2", "Alex", Some("alex@y.com"));
        let trailers = coauthor_trailers(&sam, &[sam.clone(), alex.clone(), alex_dup]);
        assert_eq!(trailers, vec!["Co-authored-by: Alex <alex@y.com>".to_string()]);
    }

    #[test]
    fn three_person_thread_credits_all_others() {
        let sam = actor("1", "Sam", None);
        let alex = actor("2", "Alex", Some("alex@y.com"));
        let jo = actor("3", "Jo", Some("jo@z.com"));
        let trailers = coauthor_trailers(&sam, &[alex, jo]);
        assert_eq!(trailers.len(), 2);
        assert!(trailers[0].contains("Alex <alex@y.com>"));
        assert!(trailers[1].contains("Jo <jo@z.com>"));
    }

    #[test]
    fn note_is_empty_without_coauthors() {
        let sam = actor("1", "Sam", None);
        assert!(commit_attribution_note(&sam, &[sam.clone()]).is_empty());
        assert!(commit_attribution_note(&sam, &[actor("2", "Al", None)]).contains("Co-authored-by"));
    }
}
