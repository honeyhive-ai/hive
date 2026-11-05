//! Markdown → plain text for push previews.
//!
//! Agent replies are GitHub-flavored markdown. A notification body can't
//! render it, and the preview is cut at [`super::PREVIEW_CHARS`], so the raw
//! syntax (table pipes and `|---|` rows, `**`, code fences, link URLs) would
//! eat most of the budget. [`markdown_to_plain`] keeps the words and drops
//! the markup:
//!
//! - headings, blockquote markers, horizontal rules and code fences go;
//!   code inside a fence stays as text;
//! - list items become `• item` (ordered items keep their number, task
//!   items lose their checkbox);
//! - a table becomes one line per body row, `Header: value · Header: value`,
//!   rows joined with `;` so they stay apart once whitespace is collapsed;
//! - inline emphasis, strikethrough and code backticks are removed, links
//!   and images keep only their text, backslash escapes are resolved.
//!
//! It is deliberately small and forgiving: anything it doesn't recognize
//! passes through unchanged.

/// Lines of plain text for `md`.
pub fn markdown_to_plain(md: &str) -> String {
    let lines: Vec<&str> = md.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    let mut fence: Option<String> = None;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if let Some(open) = &fence {
            if trimmed.starts_with(open.as_str()) && trimmed.trim_start_matches(open.chars().next().unwrap_or('`')).trim().is_empty() {
                fence = None;
            } else {
                out.push(line.trim_end().to_string());
            }
            i += 1;
            continue;
        }
        if let Some(open) = fence_open(trimmed) {
            fence = Some(open);
            i += 1;
            continue;
        }
        // A table: a pipe row followed by a separator row.
        if i + 1 < lines.len() && is_pipe_row(line) && is_separator_row(lines[i + 1]) {
            let header: Vec<String> = split_row(line).iter().map(|c| inline_plain(c)).collect();
            let mut rows = Vec::new();
            let mut j = i + 2;
            while j < lines.len() && is_pipe_row(lines[j]) && !lines[j].trim().is_empty() {
                let cells = split_row(lines[j]);
                let parts: Vec<String> = cells
                    .iter()
                    .enumerate()
                    .map(|(k, c)| (header.get(k).map(String::as_str).unwrap_or(""), inline_plain(c)))
                    .filter(|(_, v)| !v.is_empty())
                    .map(|(h, v)| if h.is_empty() { v } else { format!("{h}: {v}") })
                    .collect();
                if !parts.is_empty() {
                    rows.push(parts.join(" · "));
                }
                j += 1;
            }
            if rows.is_empty() {
                // A header with no body: show the header names.
                rows.push(header.into_iter().filter(|h| !h.is_empty()).collect::<Vec<_>>().join(" · "));
            }
            let n = rows.len();
            for (k, r) in rows.into_iter().enumerate() {
                out.push(if k + 1 < n { format!("{r};") } else { r });
            }
            i = j;
            continue;
        }
        if is_rule(trimmed) {
            i += 1;
            continue;
        }
        out.push(block_line(trimmed));
        i += 1;
    }
    let mut s = out.join("\n");
    let t = s.trim_end().len();
    s.truncate(t);
    s
}

/// ```` ``` ```` / `~~~` (with an optional info string) opens a fence;
/// returns the run that closes it.
fn fence_open(t: &str) -> Option<String> {
    for ch in ['`', '~'] {
        let n = t.chars().take_while(|c| *c == ch).count();
        if n >= 3 {
            let info = &t[n..];
            if ch == '`' && info.contains('`') {
                return None;
            }
            return Some(std::iter::repeat_n(ch, n).collect());
        }
    }
    None
}

fn is_rule(t: &str) -> bool {
    let t = t.trim();
    for ch in ['-', '*', '_'] {
        if t.chars().filter(|c| *c == ch).count() >= 3 && t.chars().all(|c| c == ch || c == ' ') {
            return true;
        }
    }
    false
}

fn is_pipe_row(line: &str) -> bool {
    line.contains('|')
}

fn is_separator_row(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('-') {
        return false;
    }
    let cells = split_row(t);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim();
            !c.is_empty() && c.contains('-') && c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
        })
}

/// The cells of a `| a | b |` row (outer pipes optional, `\|` escapes a pipe,
/// pipes inside inline code don't split).
fn split_row(line: &str) -> Vec<String> {
    let mut t = line.trim();
    if let Some(r) = t.strip_prefix('|') {
        t = r;
    }
    if t.ends_with('|') && !t.ends_with("\\|") {
        t = &t[..t.len() - 1];
    }
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_code = false;
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '`' => {
                in_code = !in_code;
                cur.push(c);
            }
            '|' if !in_code => cells.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

/// One non-table, non-fence line: block markers off, inline markup off.
fn block_line(t: &str) -> String {
    let mut t = t;
    // Blockquote markers (possibly nested).
    while let Some(r) = t.strip_prefix('>') {
        t = r.trim_start();
    }
    // ATX heading.
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && (t.len() == hashes || t[hashes..].starts_with(' ')) {
        let h = t[hashes..].trim().trim_end_matches('#').trim_end();
        return inline_plain(h);
    }
    // Unordered list item.
    for m in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(m) {
            return format!("• {}", inline_plain(strip_task(r)));
        }
    }
    // Ordered list item: digits then `.` or `)`.
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits) {
        let rest = &t[digits..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return format!("{}. {}", &t[..digits], inline_plain(strip_task(&rest[2..])));
        }
    }
    inline_plain(t)
}

fn strip_task(r: &str) -> &str {
    for b in ["[ ] ", "[x] ", "[X] "] {
        if let Some(x) = r.strip_prefix(b) {
            return x;
        }
    }
    r
}

/// Inline markdown → text: emphasis/strikethrough markers dropped, code
/// spans unwrapped, `[text](url)` / `![alt](url)` → text, `<url>` → url,
/// backslash escapes resolved.
pub fn inline_plain(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match ch {
            '\\' if i + 1 < c.len() && c[i + 1].is_ascii_punctuation() => {
                out.push(c[i + 1]);
                i += 2;
            }
            '`' => {
                // A code span: the same run of backticks closes it.
                let run = c[i..].iter().take_while(|x| **x == '`').count();
                let body_start = i + run;
                let mut j = body_start;
                let mut close = None;
                while j < c.len() {
                    if c[j] == '`' {
                        let r = c[j..].iter().take_while(|x| **x == '`').count();
                        if r == run {
                            close = Some(j);
                            break;
                        }
                        j += r;
                    } else {
                        j += 1;
                    }
                }
                match close {
                    Some(j) => {
                        let body: String = c[body_start..j].iter().collect();
                        out.push_str(body.trim());
                        i = j + run;
                    }
                    None => {
                        out.extend(&c[i..body_start]);
                        i = body_start;
                    }
                }
            }
            '!' if c.get(i + 1) == Some(&'[') => match link_at(&c, i + 1) {
                Some((text, end)) => {
                    out.push_str(&inline_plain(&text));
                    i = end;
                }
                None => {
                    out.push(ch);
                    i += 1;
                }
            },
            '[' => match link_at(&c, i) {
                Some((text, end)) => {
                    out.push_str(&inline_plain(&text));
                    i = end;
                }
                None => {
                    out.push(ch);
                    i += 1;
                }
            },
            '<' => {
                // Autolink <https://…> / <mailto:…>.
                let end = c[i + 1..].iter().position(|x| *x == '>' || x.is_whitespace()).map(|p| p + i + 1);
                match end {
                    Some(e) if c[e] == '>' => {
                        let inner: String = c[i + 1..e].iter().collect();
                        if inner.contains("://") || inner.starts_with("mailto:") {
                            out.push_str(inner.trim_start_matches("mailto:"));
                            i = e + 1;
                        } else {
                            out.push(ch);
                            i += 1;
                        }
                    }
                    _ => {
                        out.push(ch);
                        i += 1;
                    }
                }
            }
            '*' | '_' | '~' => {
                let run = c[i..].iter().take_while(|x| **x == ch).count();
                let before = if i == 0 { None } else { Some(c[i - 1]) };
                let after = c.get(i + run).copied();
                let word = |x: Option<char>| x.is_some_and(|x| x.is_alphanumeric());
                let space = |x: Option<char>| x.is_none_or(char::is_whitespace);
                let markup = match ch {
                    // `~~strike~~`; a lone `~` is text (“~5 min”).
                    '~' => run >= 2 && !(space(before) && space(after)),
                    // Intraword `_` is text (snake_case).
                    '_' => !((word(before) && word(after)) || (space(before) && space(after))),
                    // A `*` with spaces on both sides is text (“2 * 3”).
                    _ => !(space(before) && space(after)),
                };
                if !markup {
                    out.extend(&c[i..i + run]);
                }
                i += run;
            }
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// `[text](dest)` starting at `c[i] == '['`: the text and the index after `)`.
fn link_at(c: &[char], i: usize) -> Option<(String, usize)> {
    let mut depth = 0;
    let mut j = i;
    while j < c.len() {
        match c[j] {
            '\\' => j += 1,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    if j >= c.len() || c.get(j + 1) != Some(&'(') {
        return None;
    }
    // The destination may hold balanced parentheses (`a_(b)`).
    let mut depth = 1;
    let mut k = j + 2;
    while k < c.len() {
        match c[k] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((c[i + 1..j].iter().collect(), k + 1));
                }
            }
            _ => {}
        }
        k += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_become_header_value_rows() {
        let md = "Here are the hosts:\n\n| Host | CPU | Status |\n|:-----|----:|:------:|\n| **alpha** | 12% | `ok` |\n| beta | 87% | [hot](https://example.com) |\n\nDone.";
        assert_eq!(
            markdown_to_plain(md),
            "Here are the hosts:\n\nHost: alpha · CPU: 12% · Status: ok;\nHost: beta · CPU: 87% · Status: hot\n\nDone."
        );
    }

    #[test]
    fn tables_without_outer_pipes_and_with_escaped_pipes() {
        let md = "a | b\n--- | ---\nx \\| y | `p|q`";
        assert_eq!(markdown_to_plain(md), "a: x | y · b: p|q");
    }

    #[test]
    fn empty_cells_are_skipped_and_header_only_tables_list_columns() {
        assert_eq!(markdown_to_plain("| A | B |\n|---|---|\n| 1 |  |"), "A: 1");
        assert_eq!(markdown_to_plain("| A | B |\n|---|---|"), "A · B");
    }

    #[test]
    fn block_markup_is_removed() {
        let md = "# Summary ##\n> quoted **text**\n\n- one\n* two\n  + nested\n1. first\n2) second\n- [x] done\n- [ ] todo\n\n---\n***\nend";
        assert_eq!(
            markdown_to_plain(md),
            "Summary\nquoted text\n\n• one\n• two\n• nested\n1. first\n2. second\n• done\n• todo\n\nend"
        );
    }

    #[test]
    fn code_fences_drop_but_code_stays() {
        let md = "Run:\n```bash\ncargo test -p hive-cli\n```\n~~~\nx\n~~~\nok";
        assert_eq!(markdown_to_plain(md), "Run:\ncargo test -p hive-cli\nx\nok");
    }

    #[test]
    fn inline_markup_is_removed_but_text_is_kept() {
        assert_eq!(inline_plain("**bold** *it* _em_ __strong__ ~~gone~~ `code`"), "bold it em strong gone code");
        assert_eq!(inline_plain("see [the docs](https://x.test/a_(b)) and ![logo](l.png)"), "see the docs and logo");
        assert_eq!(inline_plain("<https://example.com> \\*literal\\*"), "https://example.com *literal*");
        // Things that look like markup but are text.
        assert_eq!(inline_plain("snake_case_name 2 * 3 ~5 min a_b"), "snake_case_name 2 * 3 ~5 min a_b");
        assert_eq!(inline_plain("``a `tick` b``"), "a `tick` b");
        assert_eq!(inline_plain("unclosed `tick and [bracket"), "unclosed `tick and [bracket");
        assert_eq!(inline_plain("emoji 🐝 ✅ stay"), "emoji 🐝 ✅ stay");
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(markdown_to_plain("Done: the build is green."), "Done: the build is green.");
        assert_eq!(markdown_to_plain("a | b without a separator"), "a | b without a separator");
    }
}
