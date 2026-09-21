/// A single added line extracted from a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedLine {
    pub path: String,
    pub line_no: usize,
    pub text: String,
    pub is_new_file: bool,
}

/// Parse unified diff text and keep only added (`+`) lines.
/// Context and deletions are ignored — they are not leaving the machine.
///
/// Returns `Err` if a `+` content line was seen without a resolvable path
/// (a truncated or unknown diff would otherwise look like a clean scan).
pub fn parse_unified_diff(raw: &str) -> Result<Vec<AddedLine>, String> {
    let mut out = Vec::new();
    let mut path = String::new();
    let mut new_line: usize = 0;
    let mut is_new_file = false;
    let mut skip_file = false;
    let mut plus_without_path = false;
    let mut in_hunk = false;
    let mut hunk_lines_left: usize = 0;

    for raw_line in raw.split_inclusive('\n') {
        let line = raw_line.trim_end_matches(['\n', '\r']);

        if let Some(rest) = line.strip_prefix("diff --git ") {
            skip_file = false;
            is_new_file = false;
            in_hunk = false;
            hunk_lines_left = 0;
            path = parse_git_dst(rest).unwrap_or_default();
            continue;
        }
        if line.starts_with("new file mode") {
            is_new_file = true;
            continue;
        }
        if line.starts_with("deleted file mode") {
            skip_file = true;
            path.clear();
            continue;
        }
        if line.starts_with("rename to ") {
            path = strip_git_side(line["rename to ".len()..].trim());
            continue;
        }
        if line.starts_with("copy to ") {
            path = strip_git_side(line["copy to ".len()..].trim());
            continue;
        }
        if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
            skip_file = true;
            continue;
        }
        if !in_hunk && is_plus_file_header(line) {
            let rest = line[3..].trim_start();
            let rest = cut_tab(rest);
            if rest != "/dev/null" {
                path = strip_git_side(rest);
            }
            continue;
        }
        if line.starts_with("--- ") {
            continue;
        }
        if skip_file {
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            new_line = parse_hunk_new_start(rest).unwrap_or(0);
            hunk_lines_left = parse_hunk_added_count(rest);
            in_hunk = hunk_lines_left > 0;
            continue;
        }

        if in_hunk {
            let is_plus = line.starts_with('+');
            let is_context = line.starts_with(' ') || line.is_empty();
            if is_plus || is_context {
                if hunk_lines_left > 0 {
                    hunk_lines_left -= 1;
                }
                if hunk_lines_left == 0 {
                    in_hunk = false;
                }
            }
        }

        if line.starts_with('+') {
            let text = line[1..].to_string();
            if path.is_empty() {
                plus_without_path = true;
                continue;
            }
            if merge_continuation(&mut out, &path, &text) {
                new_line = new_line.saturating_add(1);
                continue;
            }
            out.push(AddedLine {
                path: path.clone(),
                line_no: new_line,
                text,
                is_new_file,
            });
            new_line = new_line.saturating_add(1);
        } else if line.starts_with(' ') {
            new_line = new_line.saturating_add(1);
        } else if line.starts_with('\\') {
            // "\ No newline at end of file" — not content.
        }
    }

    if plus_without_path && out.is_empty() {
        return Err("unified diff has added lines but no file path (truncated or unknown format)".into());
    }
    if plus_without_path {
        return Err("unified diff has added lines without a resolvable path".into());
    }
    Ok(out)
}

/// `+++` is a file header only when `+++` is followed by whitespace
/// (`+++ b/foo`, `+++ /dev/null`, `+++ "b/my file"`).
/// `+++i;` is an added line whose payload is `++i;`.
fn is_plus_file_header(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("+++") else {
        return false;
    };
    rest.starts_with(' ') || rest.starts_with('\t')
}

fn merge_continuation(out: &mut [AddedLine], path: &str, next: &str) -> bool {
    let Some(last) = out.last_mut() else {
        return false;
    };
    if last.path != path {
        return false;
    }
    if !is_line_continuation(&last.text) {
        return false;
    }
    last.text.pop();
    last.text.push_str(next);
    true
}

fn is_line_continuation(text: &str) -> bool {
    if !text.ends_with('\\') {
        return false;
    }
    // raw `\\` is an escaped backslash, not a continuation
    !text.ends_with("\\\\")
}

fn parse_git_dst(rest: &str) -> Option<String> {
    let parts = tokenize_git_args(rest);
    let dst = parts.last()?;
    let stripped = strip_git_side(dst);
    if stripped.is_empty() {
        None
    } else {
        Some(stripped)
    }
}

fn tokenize_git_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'"' {
            i += 1;
            let mut raw = String::new();
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    raw.push('\\');
                    raw.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                raw.push(bytes[i] as char);
                i += 1;
            }
            out.push(unescape_git(&raw));
        } else {
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(s[start..i].to_string());
        }
    }
    out
}

fn strip_git_side(s: &str) -> String {
    let s = cut_tab(s.trim());
    let s = unquote_git(&s);
    if let Some(rest) = s.strip_prefix("b/") {
        return rest.to_string();
    }
    if let Some(rest) = s.strip_prefix("a/") {
        return rest.to_string();
    }
    s
}

fn unquote_git(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        unescape_git(&s[1..s.len() - 1])
    } else {
        s.to_string()
    }
}

fn unescape_git(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\\' || i + 1 >= chars.len() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        i += 1;
        match chars[i] {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            'a' => out.push('\u{0007}'),
            'b' => out.push('\u{0008}'),
            'f' => out.push('\u{000c}'),
            'v' => out.push('\u{000b}'),
            c if c.is_ascii_digit() => {
                let mut val = 0u32;
                let mut n = 0;
                while i < chars.len() && n < 3 && chars[i].is_ascii_digit() {
                    val = val * 8 + chars[i].to_digit(10).unwrap_or(0);
                    i += 1;
                    n += 1;
                }
                if let Some(ch) = char::from_u32(val) {
                    out.push(ch);
                }
                continue;
            }
            other => {
                out.push(other);
            }
        }
        i += 1;
    }
    out
}

fn cut_tab(s: &str) -> &str {
    match s.find('\t') {
        Some(i) => &s[..i],
        None => s,
    }
}

fn parse_hunk_new_start(rest: &str) -> Option<usize> {
    let plus = rest.split_whitespace().find(|p| p.starts_with('+'))?;
    let num = plus.trim_start_matches('+').split(',').next()?;
    num.parse().ok()
}

fn parse_hunk_added_count(rest: &str) -> usize {
    let Some(plus_part) = rest.split_whitespace().find(|p| p.starts_with('+')) else {
        return 0;
    };
    let mut parts = plus_part.trim_start_matches('+').split(',');
    parts.next(); // skip line number
    if let Some(count_str) = parts.next() {
        count_str.parse().unwrap_or(0)
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(diff: &str) -> Vec<AddedLine> {
        parse_unified_diff(diff).unwrap_or_else(|e| panic!("parse failed: {e}\n{diff}"))
    }

    #[test]
    fn extracts_only_added_lines_with_correct_numbers() {
        let diff = "\
diff --git a/src/app.rs b/src/app.rs
index 111..222 100644
--- a/src/app.rs
+++ b/src/app.rs
@@ -10,0 +11,2 @@ fn main() {
+let key = \"AKIAAAAAAAAAAAAAAAAA\";
+println!(\"{key}\");
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].path, "src/app.rs");
        assert_eq!(lines[0].line_no, 11);
        assert!(lines[0].text.contains("AKIAAAA"));
        assert_eq!(lines[1].line_no, 12);
    }

    #[test]
    fn detects_new_env_file() {
        let diff = "\
diff --git a/.env b/.env
new file mode 100644
index 000..111
--- /dev/null
+++ b/.env
@@ -0,0 +1,1 @@
+DATABASE_URL=postgres://u:p@host/db
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].is_new_file);
        assert_eq!(lines[0].path, ".env");
    }

    #[test]
    fn added_line_starting_with_plus_plus_is_content() {
        let diff = "\
diff --git a/x.c b/x.c
--- a/x.c
+++ b/x.c
@@ -1,0 +2 @@
+++i;
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "++i;");
        assert_eq!(lines[0].path, "x.c");
    }

    #[test]
    fn quoted_path_with_space() {
        let diff = "\
diff --git \"a/my file.env\" \"b/my file.env\"
new file mode 100644
--- /dev/null
+++ \"b/my file.env\"
@@ -0,0 +1 @@
+SECRET=1
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].path, "my file.env");
        assert!(lines[0].is_new_file);
    }

    #[test]
    fn gnu_timestamp_is_stripped() {
        let diff = "\
--- a/foo.rs
+++ b/foo.rs\t2024-01-01 00:00:00.000000000 +0000
@@ -0,0 +1 @@
+fn x() {}
";
        let lines = parse(diff);
        assert_eq!(lines[0].path, "foo.rs");
    }

    #[test]
    fn binary_files_emit_nothing() {
        let diff = "\
diff --git a/a.bin b/a.bin
Binary files a/a.bin and b/a.bin differ
";
        assert!(parse(diff).is_empty());
    }

    #[test]
    fn deleted_file_emits_nothing() {
        let diff = "\
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-secret
";
        assert!(parse(diff).is_empty());
    }

    #[test]
    fn rename_with_hunk_uses_new_path() {
        let diff = "\
diff --git a/old.rs b/new.rs
similarity index 80%
rename from old.rs
rename to new.rs
--- a/old.rs
+++ b/new.rs
@@ -1 +1 @@
-old
+AKIAAAAAAAAAAAAAAAAA
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].path, "new.rs");
        assert_eq!(lines[0].line_no, 1);
    }

    #[test]
    fn hunk_without_comma_starts_at_one() {
        let diff = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
+only
";
        let lines = parse(diff);
        assert_eq!(lines[0].line_no, 1);
        assert_eq!(lines[0].text, "only");
    }

    #[test]
    fn no_newline_marker_does_not_invent_a_line() {
        let diff = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -0,0 +1 @@
+hello
\\ No newline at end of file
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "hello");
    }

    #[test]
    fn plus_without_path_is_an_error() {
        let err = parse_unified_diff("+SECRET=1\n").unwrap_err();
        assert!(err.contains("path"), "{err}");
    }

    #[test]
    fn continuation_backslash_joins_next_added_line() {
        let diff = "\
diff --git a/.env b/.env
--- a/.env
+++ b/.env
@@ -0,0 +1,2 @@
+SECRET=foo\\
+bar
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "SECRET=foobar");
    }

    #[test]
    fn spoof_plus_plus_in_hunk_does_not_change_path() {
        let diff = "\
diff --git a/src/test.rs b/src/test.rs
--- a/src/test.rs
+++ b/src/test.rs
@@ -1,0 +1,3 @@
+let fake = r#\"
+++ b/node_modules/bypass
+let secret = \"AKIAIOSFODNN7EXAMPLE\";
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].path, "src/test.rs");
        assert_eq!(lines[1].path, "src/test.rs");
        assert_eq!(lines[1].text, "++ b/node_modules/bypass");
        assert_eq!(lines[2].path, "src/test.rs");
        assert!(lines[2].text.contains("AKIA"));
    }

    #[test]
    fn multiple_files_reset_hunk_state() {
        let diff = "\
diff --git a/file1 b/file1
--- a/file1
+++ b/file1
@@ -1 +1 @@
+one
diff --git a/file2 b/file2
--- a/file2
+++ b/file2
@@ -1 +1 @@
+two
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].path, "file1");
        assert_eq!(lines[0].text, "one");
        assert_eq!(lines[1].path, "file2");
        assert_eq!(lines[1].text, "two");
    }

    #[test]
    fn multiple_hunks_retain_hunk_state() {
        let diff = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1 +1 @@
+first
@@ -10 +10 @@
+++ b/ignore
+second
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].path, "f");
        assert_eq!(lines[1].path, "f");
        assert_eq!(lines[1].text, "++ b/ignore");
        assert_eq!(lines[2].path, "f");
        assert_eq!(lines[2].text, "second");
    }

    #[test]
    fn generic_unified_diff_multiple_files() {
        let diff = "\
--- old1
+++ new1
@@ -1 +1 @@
+a
--- old2
+++ new2
@@ -1 +1 @@
+b
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].path, "new1");
        assert_eq!(lines[0].text, "a");
        assert_eq!(lines[1].path, "new2");
        assert_eq!(lines[1].text, "b");
    }

    #[test]
    fn generic_unified_diff_secret_after_file_change() {
        let diff = "\
--- node_modules/bypass/old.js
+++ node_modules/bypass/new.js
@@ -1 +1 @@
+safe
--- src/main.rs
+++ src/main.rs
@@ -1 +1 @@
+secret
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].path, "node_modules/bypass/new.js");
        assert_eq!(lines[1].path, "src/main.rs");
        assert_eq!(lines[1].text, "secret");
    }

    #[test]
    fn generic_unified_diff_new_file() {
        let diff = "\
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1 @@
+hello
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].path, "src/new.rs");
    }

    #[test]
    fn generic_unified_diff_deleted_file() {
        let diff = "\
--- a/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-secret
";
        let lines = parse(diff);
        assert!(lines.is_empty());
    }

    #[test]
    fn combined_spoofing_and_generic_unified_diff() {
        let diff = "\
--- a/file1
+++ b/file1
@@ -1,2 +1,3 @@
 context
+++ b/bypass
+secret1
--- a/file2
+++ b/file2
@@ -10,2 +10,2 @@
-old
+secret2
";
        let lines = parse(diff);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].path, "file1");
        assert_eq!(lines[0].text, "++ b/bypass");
        assert_eq!(lines[1].path, "file1");
        assert_eq!(lines[1].text, "secret1");
        assert_eq!(lines[2].path, "file2");
        assert_eq!(lines[2].text, "secret2");
    }
}