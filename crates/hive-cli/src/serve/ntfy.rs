//! Push notifications through a self-hosted ntfy server.
//!
//! `POST <url>/<topic>` with `Title`, `Click: hive://chat/<chatId>` (the app
//! opens the chat on tap) and `Tags`. The body is a short, fixed-shape line —
//! who did what, in which chat — never message contents, so a reply carrying a
//! secret never leaves the tailnet through a notification. An optional access
//! token (`HIVE_NTFY_TOKEN`) is sent as a bearer token.

use std::time::Duration;

use base64::Engine;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Ntfy {
    pub url: String,
    pub topic: String,
    pub token: Option<String>,
}

/// One notification.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub chat_id: Uuid,
    pub tags: &'static str,
}

impl Ntfy {
    /// The topic URL (`<url>/<topic>`).
    pub fn endpoint(&self) -> String {
        format!("{}/{}", self.url.trim_end_matches('/'), self.topic.trim_matches('/'))
    }

    pub async fn send(&self, client: &reqwest::Client, n: &Notice) {
        let mut req = client
            .post(self.endpoint())
            .timeout(Duration::from_secs(5))
            .header("Title", header_text(&n.title))
            .header("Click", format!("hive://chat/{}", n.chat_id))
            .header("Tags", n.tags)
            .body(n.body.clone());
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => eprintln!("ntfy: HTTP {}", r.status()),
            Err(e) => eprintln!("ntfy: {}", e.without_url()),
        }
    }
}

/// A header-safe rendering of `s`: plain printable ASCII passes through;
/// anything else is sent as an RFC 2047 encoded word, which ntfy decodes.
pub fn header_text(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    if clean.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        clean
    } else {
        format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(clean.as_bytes()))
    }
}

fn short(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

/// The notice for a finished agent reply.
pub fn reply_notice(agent: &str, chat_title: &str, chat_id: Uuid, failed: bool) -> Notice {
    let chat = short(if chat_title.trim().is_empty() { "Untitled" } else { chat_title }, 60);
    let agent = short(agent, 40);
    Notice {
        title: if failed { format!("{agent} hit an error") } else { format!("{agent} replied") },
        body: format!("in “{chat}”"),
        chat_id,
        tags: if failed { "warning" } else { "speech_balloon" },
    }
}

/// The notice for a new proposal awaiting review.
pub fn proposal_notice(title: &str, chat_title: &str, chat_id: Uuid) -> Notice {
    let chat = short(if chat_title.trim().is_empty() { "Untitled" } else { chat_title }, 60);
    Notice {
        title: "Change ready for review".into(),
        body: format!("{} — in “{chat}”", short(title, 80)),
        chat_id,
        tags: "memo",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_titles_pass_and_unicode_is_encoded() {
        assert_eq!(header_text("coder replied"), "coder replied");
        let enc = header_text("café ☕");
        assert!(enc.starts_with("=?UTF-8?B?") && enc.ends_with("?="));
        assert_eq!(header_text("a\nb"), "ab");
    }

    #[test]
    fn notices_carry_no_message_content_and_link_the_chat() {
        let id = Uuid::new_v4();
        let n = reply_notice("@coder", "Fix the build", id, false);
        assert_eq!(n.title, "@coder replied");
        assert_eq!(n.body, "in “Fix the build”");
        assert_eq!(n.chat_id, id);
        let p = proposal_notice("@coder proposed changes to 1 file", "", id);
        assert!(p.body.contains("Untitled"));
    }

    #[test]
    fn endpoint_joins_cleanly() {
        let n = Ntfy { url: "http://h:8095/".into(), topic: "/t1".into(), token: None };
        assert_eq!(n.endpoint(), "http://h:8095/t1");
    }
}
