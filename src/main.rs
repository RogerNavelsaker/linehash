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
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 2000)]
        limit: usize,
    },
    Edit {
        path: PathBuf,
    },
    Skill,
}

#[derive(Serialize)]
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
        Commands::Read { path, after, limit } => read_command(&path, after.as_deref(), limit),
        Commands::Edit { path } => edit_command(&path),
        Commands::Skill => {
            println!("{SKILL_MARKDOWN}");
            Ok(())
        }
    }
}

fn read_command(path: &Path, after: Option<&str>, limit: usize) -> Result<(), String> {
    let content = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    for line in window(&lines, after, limit)? {
        println!("{}", serde_json::to_string(&line).unwrap());
    }
    Ok(())
}

fn edit_command(path: &Path) -> Result<(), String> {
    match apply_edits(path) {
        Ok(()) => println!("{}", json!({"status": "ok"})),
        Err(EditError { detail, message }) => {
            println!(
                "{}",
                json!({"status": "error", "detail": detail, "message": message})
            );
            process::exit(1);
        }
    }
    Ok(())
}

fn apply_edits(path: &Path) -> Result<(), EditError> {
    let input = read_stdin()?;
    let mut edits = parse_edits(&input)?;
    let content = fs::read_to_string(path).map_err(|error| EditError::failed(error.to_string()))?;
    let trailing_newline = content.ends_with('\n');
    let mut lines = split_file_lines(&content, trailing_newline);
    let anchors = anchors_for(&lines);

    edits.sort_by_key(|edit| std::cmp::Reverse(edit_start_index(edit, &anchors).unwrap_or(0)));
    for edit in edits {
        apply_edit(&mut lines, &anchors, edit)?;
    }

    let mut output = lines.join("\n");
    if trailing_newline {
        output.push('\n');
    }
    fs::write(path, output).map_err(|error| EditError::failed(error.to_string()))
}

fn apply_edit(lines: &mut Vec<String>, anchors: &[String], edit: Edit) -> Result<(), EditError> {
    match edit.op.as_str() {
        "replace" => replace_lines(lines, anchors, edit),
        "insert_before" => insert_before(lines, anchors, edit),
        "insert_after" => insert_after(lines, anchors, edit),
        _ => Err(EditError::input("unknown op")),
    }
}

fn replace_lines(lines: &mut Vec<String>, anchors: &[String], edit: Edit) -> Result<(), EditError> {
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
    let start = anchor_index(anchors, &from)?;
    let end = anchor_index(anchors, &to)?;
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
    limit: usize,
) -> Result<Vec<AnchoredLine>, String> {
    let anchors = anchors_for(lines);
    let start = match after {
        Some(anchor) => anchors
            .iter()
            .position(|candidate| candidate == anchor)
            .map(|index| index + 1)
            .ok_or_else(|| "anchor_invalid".to_string())?,
        None => 0,
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
