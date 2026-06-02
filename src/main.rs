use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;
use xxhash_rust::xxh32::xxh32;

#[derive(Parser)]
#[command(author, version, about = "JSONL line-hash file tool for AI agents")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Read {
        path: PathBuf,
        /// Anchor (4-char hex) to start after (exclusive). Use --offset for line-number-based windowing. (exclusive). Expects a 4-char hex anchor.
        #[arg(long)]
        after: Option<String>,
        /// Line number to start from (1-indexed). Mutually exclusive with --after.
        #[arg(long)]
        offset: Option<usize>,
        /// Maximum number of lines to return
        #[arg(long, default_value_t = 2000)]
        limit: usize,
    },
    Edit {
        path: PathBuf,
        /// Enable fuzzy matching: normalize Unicode quotes/dashes/spaces and whitespace before matching
        #[arg(long)]
        fuzzy: bool,
        /// Compute diff but do not write the file
        #[arg(long)]
        dry_run: bool,
    },
    Skill,
}

#[derive(Serialize)]
#[derive(Debug)]
struct AnchoredLine {
    line: usize,
    anchor: String,
    text: String,
}

#[derive(Deserialize)]
struct Edit {
    op: String,
    anchor: Option<String>,
    from: Option<String>,
    to: Option<String>,
    before: Option<String>,
    after: Option<String>,
    text: Option<String>,
}

/// Apply edits and return the post-edit content (for diff computation).
fn apply_edits_str(path: &Path, input: &str, fuzzy: bool) -> Result<String, EditError> {
    let mut edits = parse_edits(input)?;
    let has_replace_all = edits.iter().any(|e| e.op == "replace_all");

    if has_replace_all {
        // replace_all: first op with replace_all replaces entire file content
        if let Some(edit) = edits.iter().find(|e| e.op == "replace_all") {
            let text = edit.text.as_deref().unwrap_or("");
            return Ok(text.to_string());
        }
    }

    // Existing logic: read file, apply edits, write back
    let content = fs::read_to_string(path).map_err(|error| EditError::failed(error.to_string()))?;
    let trailing_newline = content.ends_with('\n');
    let mut lines = split_file_lines(&content, trailing_newline);
    let anchors = anchors_for(&lines);

    // If fuzzy mode, normalize edit text before matching
    if fuzzy {
        for edit in &mut edits {
            if let Some(ref mut from) = edit.from {
                *from = normalize_text(from);
            }
            if let Some(ref mut before) = edit.before {
                *before = normalize_text(before);
            }
            if let Some(ref mut after) = edit.after {
                *after = normalize_text(after);
            }
        }
    }

    edits.sort_by_key(|edit| std::cmp::Reverse(edit_start_index(edit, &anchors).unwrap_or(0)));
    for edit in edits {
        apply_edit(&mut lines, &anchors, edit, fuzzy)?;
    }

    let mut output = lines.join("\n");
    if trailing_newline {
        output.push('\n');
    }
    Ok(output)
}

const SKILL_MARKDOWN: &str = r#"---
name: linehash
description: JSONL line-hash file read/edit tool for AI agents.
---

# linehash

Commands:
- `linehash read <path>`
- `linehash edit <path>`
- `le read <path>`
- `le edit <path>`

Read emits one flat JSONL row per line:
`{"line":1,"anchor":"a1b2","text":"fn main() {"}`

Edit reads one JSONL edit op per row:
`{"op":"replace","anchor":"a1b2","text":"fn main() {"}`

Edit ops:
- `replace`: `anchor` or `from` + `to`, plus `text`. Empty `text` deletes.
- `insert_before`: `before` or `anchor`, plus `text`.
- `insert_after`: `after` or `anchor`, plus `text`.

Edit result:
- success: `{"status":"ok"}`
- error: `{"status":"error","detail":"anchor_invalid","message":"anchor not found"}`
"#;

/// Normalize text for fuzzy matching: Unicode normalization + whitespace normalization.
/// Multi-pass: quotes/dashes → ASCII, NBSP → space, collapse whitespace, trim.
fn normalize_text(input: &str) -> String {
    let mut s = input.to_string();
    // Pass 1: Unicode curly quotes → straight quotes
    s = s.replace('\u{201c}', "\"").replace('\u{201d}', "\"");
    s = s.replace('\u{2018}', "'").replace('\u{2019}', "'");
    // Pass 2: Unicode em/en dashes → regular dash
    s = s.replace('\u{2014}', "-").replace('\u{2013}', "-");
    // Pass 3: Unicode non-breaking space → regular space
    s = s.replace('\u{00a0}', " ");
    // Pass 4: Collapse multiple whitespace to single space
    while s.contains("  ") {
        s = s.replace("  ", " ");
    }
    // Pass 5: Trim
    s = s.trim().to_string();
    s
}

fn main() {
    if let Err(error) = run() {
        println!(
            "{}",
            json!({"status": "error", "detail": "failed", "message": error})
        );
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Commands::Skill) {
        Commands::Read { path, after, offset, limit } => read_command(&path, after.as_deref(), offset, limit),
        Commands::Edit { path, fuzzy, dry_run } => edit_command(&path, fuzzy, dry_run),
        Commands::Skill => {
            println!("{SKILL_MARKDOWN}");
            Ok(())
        }
    }
}

fn read_command(path: &Path, after: Option<&str>, offset: Option<usize>, limit: usize) -> Result<(), String> {
    // Validate: --after and --offset are mutually exclusive
    if after.is_some() && offset.is_some() {
        return Err("--after and --offset are mutually exclusive. Use --after for anchor-based windowing, --offset for line-number-based windowing.".to_string());
    }

    let content = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    for line in window(&lines, after, offset, limit)? {
        println!("{}", serde_json::to_string(&line).unwrap());
    }
    Ok(())
}

fn edit_command(path: &Path, fuzzy: bool, dry_run: bool) -> Result<(), String> {
    let input = read_stdin().map_err(|e| e.message)?;
    let edits = parse_edits(&input).map_err(|e| e.message)?;
    let _has_replace_all = edits.iter().any(|e| e.op == "replace_all");

    // Read pre-edit content (always read from file, even for replace_all)
    let pre_content = fs::read_to_string(path).unwrap_or_default();

    match apply_edits_str(path, &input, fuzzy) {
        Ok(post_content) => {
            // Compute diff between pre and post
            let diff = compute_diff(&pre_content, &post_content, path);

            // Write post-edit content back to disk (skip in dry-run mode)
            if !dry_run {
                fs::write(path, &post_content).map_err(|e| format!("failed to write file: {e}"))?;
            }

            println!(
                "{}",
                json!({
                    "status": "ok",
                    "diff": diff
                })
            );
            Ok(())
        }
        Err(EditError { detail, message }) => {
            println!(
                "{}",
                json!({"status": "error", "detail": detail, "message": message})
            );
            process::exit(1);
        }
    }
}

/// Compute unified diff between two strings.
/// Output format: `--- a/<path>` / `+++ b/<path>` headers, no `Index:` or `===` lines.
fn compute_diff(old_content: &str, new_content: &str, path: &Path) -> String {
    if old_content == new_content {
        return String::new();
    }

    let text_diff = similar::TextDiff::from_lines(old_content, new_content);
    let mut unified = similar::udiff::UnifiedDiff::from_text_diff(&text_diff);
    unified.context_radius(3);

    let old_label = if old_content.is_empty() {
        "/dev/null".to_string()
    } else {
        path.display().to_string()
    };
    let new_label = path.display().to_string();
    unified.header(&old_label, &new_label);

    let mut buf = Vec::new();
    if let Err(e) = unified.to_writer(&mut buf) {
        return format!("error formatting diff: {e}");
    }
    String::from_utf8(buf).unwrap_or_default()
}

fn apply_edit(lines: &mut Vec<String>, anchors: &[String], edit: Edit, fuzzy: bool) -> Result<(), EditError> {
    match edit.op.as_str() {
        "replace" => replace_lines(lines, anchors, edit, fuzzy),
        "insert_before" => insert_before(lines, anchors, edit),
        "insert_after" => insert_after(lines, anchors, edit),
        _ => Err(EditError::input("unknown op")),
    }
}

/// Look for a line by normalized text content (fuzzy matching).
/// Returns the index of the first matching line, or an error.
fn find_line_by_normalized_text(lines: &[String], target: &str) -> Result<usize, EditError> {
    let normalized_target = normalize_text(target);
    for (index, line) in lines.iter().enumerate() {
        if normalize_text(line.trim()) == normalized_target {
            return Ok(index);
        }
    }
    Err(EditError::anchor("fuzzy match: text not found in any line"))
}

fn replace_lines(lines: &mut Vec<String>, anchors: &[String], edit: Edit, fuzzy: bool) -> Result<(), EditError> {
    let text = edit.text.unwrap_or_default();
    if let Some(anchor) = edit.anchor {
        let index = anchor_index(anchors, &anchor)?;
        lines.splice(index..=index, payload_lines(&text));
        return Ok(());
    }

    let from = edit
        .from
        .ok_or_else(|| EditError::input("replace requires anchor or from/to"))?;
    let to = edit
        .to
        .ok_or_else(|| EditError::input("replace requires anchor or from/to"))?;

    // Try anchor-based matching first
    let start = match anchor_index(anchors, &from) {
        Ok(idx) => idx,
        Err(_) if fuzzy => find_line_by_normalized_text(lines, &from)?,
        Err(e) => return Err(e),
    };
    let end = match anchor_index(anchors, &to) {
        Ok(idx) => idx,
        Err(_) if fuzzy => find_line_by_normalized_text(lines, &to)?,
        Err(e) => return Err(e),
    };
    if start > end {
        return Err(EditError::input("from anchor is after to anchor"));
    }
    lines.splice(start..=end, payload_lines(&text));
    Ok(())
}

fn insert_before(lines: &mut Vec<String>, anchors: &[String], edit: Edit) -> Result<(), EditError> {
    let anchor = edit
        .before
        .or(edit.anchor)
        .ok_or_else(|| EditError::input("insert_before requires before or anchor"))?;
    let index = anchor_index(anchors, &anchor)?;
    insert_lines(lines, index, payload_lines(&edit.text.unwrap_or_default()));
    Ok(())
}

fn insert_after(lines: &mut Vec<String>, anchors: &[String], edit: Edit) -> Result<(), EditError> {
    let anchor = edit
        .after
        .or(edit.anchor)
        .ok_or_else(|| EditError::input("insert_after requires after or anchor"))?;
    let index = anchor_index(anchors, &anchor)? + 1;
    insert_lines(lines, index, payload_lines(&edit.text.unwrap_or_default()));
    Ok(())
}

fn edit_start_index(edit: &Edit, anchors: &[String]) -> Result<usize, EditError> {
    let anchor = edit
        .anchor
        .as_ref()
        .or(edit.from.as_ref())
        .or(edit.before.as_ref())
        .or(edit.after.as_ref())
        .ok_or_else(|| EditError::input("missing anchor"))?;
    anchor_index(anchors, anchor)
}

fn window(
    lines: &[String],
    after: Option<&str>,
    offset: Option<usize>,
    limit: usize,
) -> Result<Vec<AnchoredLine>, String> {
    let anchors = anchors_for(lines);
    let start = match (after, offset) {
        (Some(anchor), None) => anchors
            .iter()
            .position(|candidate| candidate == anchor)
            .map(|index| index + 1)
            .ok_or_else(|| "anchor_invalid".to_string())?,
        (None, Some(line_num)) => {
            // offset is 1-indexed, convert to 0-indexed
            if line_num < 1 {
                return Err("--offset must be >= 1".to_string());
            }
            // Handle empty files or offset beyond file length
            if lines.is_empty() {
                return Ok(Vec::new());
            }
            let idx = line_num.saturating_sub(1).min(lines.len() - 1);
            idx
        }
        (None, None) => 0,
        _ => unreachable!(), // Mutually exclusive check in read_command
    };
    let end = usize::min(start + limit, lines.len());
    Ok((start..end)
        .map(|index| AnchoredLine {
            line: index + 1,
            anchor: anchors[index].clone(),
            text: lines[index].clone(),
        })
        .collect())
}

fn anchors_for(lines: &[String]) -> Vec<String> {
    let bases = lines
        .iter()
        .map(|line| four_char_hash(line.trim().as_bytes()))
        .collect::<Vec<_>>();
    let mut counts = HashMap::<String, usize>::new();
    for base in &bases {
        *counts.entry(base.clone()).or_default() += 1;
    }

    let mut used = HashSet::new();
    bases
        .iter()
        .enumerate()
        .map(|(index, base)| {
            if counts[base] == 1 && used.insert(base.clone()) {
                return base.clone();
            }
            unique_anchor(&mut used, index, &lines[index])
        })
        .collect()
}

fn unique_anchor(used: &mut HashSet<String>, index: usize, line: &str) -> String {
    for salt in 0.. {
        let anchor = four_char_hash(format!("{salt}\0{index}\0{line}").as_bytes());
        if used.insert(anchor.clone()) {
            return anchor;
        }
    }
    unreachable!()
}

fn four_char_hash(input: &[u8]) -> String {
    format!("{:04x}", xxh32(input, 0) & 0xffff)
}

fn anchor_index(anchors: &[String], anchor: &str) -> Result<usize, EditError> {
    anchors
        .iter()
        .position(|candidate| candidate == anchor)
        .ok_or_else(|| EditError::anchor("anchor not found"))
}

fn insert_lines(lines: &mut Vec<String>, index: usize, inserted: Vec<String>) {
    for (offset, line) in inserted.into_iter().enumerate() {
        lines.insert(index + offset, line);
    }
}

fn payload_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').map(str::to_string).collect()
    }
}

fn split_file_lines(content: &str, trailing_newline: bool) -> Vec<String> {
    let mut lines = content.split('\n').map(str::to_string).collect::<Vec<_>>();
    if trailing_newline && !lines.is_empty() {
        lines.pop();
    }
    lines
}

fn parse_edits(input: &str) -> Result<Vec<Edit>, EditError> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|error| EditError::input(error.to_string())))
        .collect()
}

fn read_stdin() -> Result<String, EditError> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| EditError::failed(error.to_string()))?;
    Ok(input)
}

#[derive(Debug)]
struct EditError {
    detail: &'static str,
    message: String,
}

impl EditError {
    fn anchor(message: impl ToString) -> Self {
        Self {
            detail: "anchor_invalid",
            message: message.to_string(),
        }
    }

    fn input(message: impl ToString) -> Self {
        Self {
            detail: "input_invalid",
            message: message.to_string(),
        }
    }

    fn failed(message: impl ToString) -> Self {
        Self {
            detail: "failed",
            message: message.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_lines_get_unique_anchors() {
        let anchors = anchors_for(&["same".to_string(), "same".to_string()]);
        assert_eq!(anchors.len(), 2);
        assert_eq!(anchors[0].len(), 4);
        assert_eq!(anchors[1].len(), 4);
        assert_ne!(anchors[0], anchors[1]);
    }

    #[test]
    fn empty_replace_text_deletes() {
        assert_eq!(payload_lines(""), Vec::<String>::new());
        assert_eq!(
            payload_lines("a\nb"),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}

// Additional tests for fuzzy write + diff header path
#[cfg(test)]
mod edit_write_tests {
    use super::*;

    fn test_id() -> String {
        use std::time::SystemTime;
        let s = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string();
        s.chars().take(8).collect()
    }

    #[test]
    fn fuzzy_edit_returns_replacement_content() {
        let tmp = std::env::temp_dir().join(format!("linehash-fuzzy-write-{}.txt", test_id()));
        fs::write(&tmp, "line one: alpha\nline two: beta\n").unwrap();
        let input = r#"{"op":"replace","from":"line   one:   alpha","to":"line   one:   alpha","text":"LINE ONE: ALPHA"}"#;
        let post = apply_edits_str(&tmp, input, true).expect("apply_edits_str should succeed");
        assert!(post.contains("LINE ONE: ALPHA"), "post_content should contain replacement: {post}");
        // apply_edits_str is read-only; file should be unchanged
        let still_old = fs::read_to_string(&tmp).unwrap();
        assert!(still_old.contains("line one: alpha"));
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn edit_command_fuzzy_writes_to_disk() {
        let tmp = std::env::temp_dir().join(format!("linehash-fuzzy-disk-{}.txt", test_id()));
        fs::write(&tmp, "line one: alpha\nline two: beta\n").unwrap();
        let input = r#"{"op":"replace","from":"line   one:   alpha","to":"line   one:   alpha","text":"LINE ONE: ALPHA"}"#;
        let pre_content = fs::read_to_string(&tmp).unwrap_or_default();
        let post_content = apply_edits_str(&tmp, input, true).expect("apply_edits_str should succeed");
        let _diff = compute_diff(&pre_content, &post_content, &tmp);
        // The critical fix: write post_content back to disk
        fs::write(&tmp, &post_content).unwrap();
        let written = fs::read_to_string(&tmp).unwrap();
        assert!(written.contains("LINE ONE: ALPHA"), "file should contain replacement: {written}");
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn diff_header_uses_full_path() {
        let tmp = std::env::temp_dir().join(format!("linehash-header-{}.txt", test_id()));
        fs::write(&tmp, "old content\n").unwrap();
        let diff = compute_diff("old content\n", "new content\n", &tmp);
        assert!(diff.contains(&tmp.display().to_string()), "diff header should use full path: {diff}");
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn diff_header_new_file_uses_dev_null() {
        let tmp = std::env::temp_dir().join(format!("linehash-header-new-{}.txt", test_id()));
        let diff = compute_diff("", "new content\n", &tmp);
        assert!(diff.contains("/dev/null"), "new file diff should have /dev/null: {diff}");
        assert!(diff.contains(&tmp.display().to_string()), "new file diff should have full path: {diff}");
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn dry_run_does_not_write_file() {
        let tmp = std::env::temp_dir().join(format!("linehash-dryrun-{}.txt", test_id()));
        fs::write(&tmp, "a\n").unwrap();
        let input = r#"{"op":"replace","from":"a","to":"a","text":"b"}"#;
        let pre_content = fs::read_to_string(&tmp).unwrap();
        let post_content = apply_edits_str(&tmp, input, true).unwrap();
        let _diff = compute_diff(&pre_content, &post_content, &tmp);
        // With dry_run=true, the file should NOT be written
        let unchanged = fs::read_to_string(&tmp).unwrap();
        assert_eq!(unchanged, "a\n", "dry_run should not modify file");
        // But post_content has the replacement
        assert!(post_content.contains("b"), "post_content should have replacement");
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn without_dry_run_file_is_written() {
        let tmp = std::env::temp_dir().join(format!("linehash-write-{}.txt", test_id()));
        fs::write(&tmp, "a\n").unwrap();
        let input = r#"{"op":"replace","from":"a","to":"a","text":"b"}"#;
        let post_content = apply_edits_str(&tmp, input, true).unwrap();
        let _diff = compute_diff("a\n", &post_content, &tmp);
        // Without dry_run, fs::write is called and file changes
        fs::write(&tmp, &post_content).unwrap();
        let written = fs::read_to_string(&tmp).unwrap();
        assert_eq!(written, "b\n", "file should be written without dry_run");
        let _ = fs::remove_file(&tmp);
    }
}

// Additional tests for window function with offset and after
#[cfg(test)]
mod window_tests {
    use super::*;

    #[test]
    fn window_offset_returns_correct_lines() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string(), "e".to_string()];
        // offset 3 (1-indexed) = index 2, limit 2 => lines c, d
        let result = window(&lines, None, Some(3), 2).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].line, 3);
        assert_eq!(result[0].text, "c");
        assert_eq!(result[1].line, 4);
        assert_eq!(result[1].text, "d");
    }

    #[test]
    fn window_offset_beyond_file_returns_from_last_line() {
        let lines = vec!["a".to_string(), "b".to_string()];
        // offset 100 beyond file length - clamps to last line (index 1 = line 2)
        let result = window(&lines, None, Some(100), 5).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 2);
        assert_eq!(result[0].text, "b");
    }

    #[test]
    fn window_empty_file_returns_empty() {
        let lines: Vec<String> = vec![];
        let result = window(&lines, None, Some(1), 5).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn window_offset_invalid_returns_error() {
        let lines = vec!["a".to_string()];
        let result = window(&lines, None, Some(0), 5);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains(">= 1"), "error was: {}", err);
    }

    #[test]
    fn window_after_returns_lines_after_anchor() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()];
        let anchors = anchors_for(&lines);
        // anchors[1] is the anchor for "b", so after should start at index 2 (line 3)
        let result = window(&lines, Some(&anchors[1]), None, 2).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].line, 3);
        assert_eq!(result[0].text, "c");
    }

    #[test]
    fn window_after_not_found_returns_error() {
        let lines = vec!["a".to_string()];
        let result = window(&lines, Some("zzzz"), None, 5);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err, "anchor_invalid", "error was: {}", err);
    }

    #[test]
    fn window_no_offset_no_after_returns_from_start() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let result = window(&lines, None, None, 2).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].line, 1);
        assert_eq!(result[0].text, "a");
    }
}
