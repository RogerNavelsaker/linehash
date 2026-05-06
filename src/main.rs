use chrono::SecondsFormat;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
        path: Option<PathBuf>,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 2000)]
        limit: usize,
    },
    Edit {
        path: Option<PathBuf>,
    },
    Skill,
}

#[derive(Deserialize)]
struct ReadRequest {
    req_id: Option<String>,
    path: PathBuf,
    after: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct EditRequest {
    req_id: Option<String>,
    path: Option<PathBuf>,
    op: String,
    anchor: Option<String>,
    from: Option<String>,
    to: Option<String>,
    before: Option<String>,
    after: Option<String>,
    text: Option<String>,
}

#[derive(Serialize)]
struct AnchoredLine {
    line: usize,
    anchor: String,
    text: String,
}

const SKILL_MARKDOWN: &str = r#"---
name: linehash
description: JSONL line-hash file read/edit tool for AI agents.
---

# linehash

Command shape:
- `linehash read`
- `linehash edit`
- `linehash skill`

Alias:
- `le read`
- `le edit`

Input is JSONL. One row = one action. Output is JSONL. One row = one result envelope.

Read:
`echo '{"path":"src/main.rs","limit":2000}' | linehash read`

Read result:
`{"req_id":"req_1","status":"completed","content":{"datetime":"...","path":"/abs/path","lines":[{"line":1,"anchor":"a1b2","text":"fn main() {"}],"truncated":false,"total_lines":10}}`

Edit:
`echo '{"path":"src/main.rs","op":"replace","anchor":"a1b2","text":"fn main() {"}' | linehash edit`

Edit ops:
- `replace`: `anchor` or `from` + `to`, plus `text`. Empty `text` deletes.
- `insert_before`: `before` or `anchor`, plus `text`.
- `insert_after`: `after` or `anchor`, plus `text`.

Fresh `linehash read` anchors are authoritative. Stale anchors return `status:"rejected"` and `detail:"anchor_invalid"`.
"#;

fn main() {
    if let Err(error) = run() {
        eprintln!(
            "{}",
            json!({ "status": "failed", "content": { "message": error } })
        );
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Commands::Skill) {
        Commands::Read { path, after, limit } => read_command(path, after, limit),
        Commands::Edit { path } => edit_command(path),
        Commands::Skill => {
            println!("{SKILL_MARKDOWN}");
            Ok(())
        }
    }
}

fn read_command(path: Option<PathBuf>, after: Option<String>, limit: usize) -> Result<(), String> {
    if let Some(path) = path {
        let request = ReadRequest {
            req_id: Some("req_1".to_string()),
            path,
            after,
            limit: Some(limit),
        };
        println!("{}", read_result(&request));
        return Ok(());
    }

    for (index, value) in read_jsonl()?.into_iter().enumerate() {
        let fallback_req_id = format!("req_{}", index + 1);
        match serde_json::from_value::<ReadRequest>(value) {
            Ok(mut request) => {
                request.req_id = Some(request.req_id.unwrap_or(fallback_req_id));
                println!("{}", read_result(&request));
            }
            Err(error) => println!("{}", rejected(&fallback_req_id, "input_invalid", error)),
        }
    }
    Ok(())
}

fn edit_command(path: Option<PathBuf>) -> Result<(), String> {
    for (index, value) in read_jsonl()?.into_iter().enumerate() {
        let fallback_req_id = format!("req_{}", index + 1);
        match serde_json::from_value::<EditRequest>(value) {
            Ok(mut request) => {
                request.req_id = Some(request.req_id.unwrap_or(fallback_req_id));
                if request.path.is_none() {
                    request.path = path.clone();
                }
                println!("{}", edit_result(&request));
            }
            Err(error) => println!("{}", rejected(&fallback_req_id, "input_invalid", error)),
        }
    }
    Ok(())
}

fn read_result(request: &ReadRequest) -> Value {
    let req_id = request.req_id.as_deref().unwrap_or("req_1");
    let limit = request.limit.unwrap_or(2000);
    match fs::read_to_string(&request.path) {
        Ok(content) => {
            let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
            match window(&lines, request.after.as_deref(), limit) {
                Ok((window, truncated)) => json!({
                    "req_id": req_id,
                    "status": "completed",
                    "content": {
                        "datetime": now(),
                        "path": absolute_path(&request.path),
                        "lines": window,
                        "after": request.after,
                        "limit": limit,
                        "truncated": truncated,
                        "total_lines": lines.len()
                    }
                }),
                Err(detail) => rejected(req_id, &detail, "anchor not found in current file"),
            }
        }
        Err(error) => failed(req_id, error),
    }
}

fn edit_result(request: &EditRequest) -> Value {
    let req_id = request.req_id.as_deref().unwrap_or("req_1");
    let Some(path) = &request.path else {
        return rejected(req_id, "input_invalid", "missing path");
    };

    match apply_edit(path, request) {
        Ok(()) => json!({
            "req_id": req_id,
            "status": "completed",
            "content": {
                "datetime": now(),
                "path": absolute_path(path)
            }
        }),
        Err(EditError::Rejected { detail, message }) => rejected(req_id, &detail, message),
        Err(EditError::Failed(message)) => failed(req_id, message),
    }
}

fn window(
    lines: &[String],
    after: Option<&str>,
    limit: usize,
) -> Result<(Vec<AnchoredLine>, bool), String> {
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
    let output = (start..end)
        .map(|index| AnchoredLine {
            line: index + 1,
            anchor: anchors[index].clone(),
            text: lines[index].clone(),
        })
        .collect();
    Ok((output, end < lines.len()))
}

fn apply_edit(path: &Path, request: &EditRequest) -> Result<(), EditError> {
    let content = fs::read_to_string(path).map_err(|error| EditError::Failed(error.to_string()))?;
    let trailing_newline = content.ends_with('\n');
    let mut lines = content.split('\n').map(str::to_string).collect::<Vec<_>>();
    if trailing_newline && !lines.is_empty() {
        lines.pop();
    }

    let operation = operation_for(request, &anchors_for(&lines))?;
    match operation {
        Operation::Replace { start, end, text } => {
            lines.splice(start..=end, split_payload(&text));
        }
        Operation::Insert { index, text } => {
            for (offset, line) in split_payload(&text).into_iter().enumerate() {
                lines.insert(index + offset, line);
            }
        }
    }

    let mut output = lines.join("\n");
    if trailing_newline {
        output.push('\n');
    }
    fs::write(path, output).map_err(|error| EditError::Failed(error.to_string()))
}

fn operation_for(request: &EditRequest, anchors: &[String]) -> Result<Operation, EditError> {
    match request.op.as_str() {
        "replace" => replace_operation(request, anchors),
        "insert_before" => {
            let anchor = request
                .before
                .as_ref()
                .or(request.anchor.as_ref())
                .ok_or_else(|| {
                    EditError::rejected("input_invalid", "insert_before requires before or anchor")
                })?;
            Ok(Operation::Insert {
                index: anchor_index(anchors, anchor)?,
                text: request.text.clone().unwrap_or_default(),
            })
        }
        "insert_after" => {
            let anchor = request
                .after
                .as_ref()
                .or(request.anchor.as_ref())
                .ok_or_else(|| {
                    EditError::rejected("input_invalid", "insert_after requires after or anchor")
                })?;
            Ok(Operation::Insert {
                index: anchor_index(anchors, anchor)? + 1,
                text: request.text.clone().unwrap_or_default(),
            })
        }
        _ => Err(EditError::rejected("input_invalid", "unknown op")),
    }
}

fn replace_operation(request: &EditRequest, anchors: &[String]) -> Result<Operation, EditError> {
    let text = request.text.clone().unwrap_or_default();
    if let Some(anchor) = &request.anchor {
        let index = anchor_index(anchors, anchor)?;
        return Ok(Operation::Replace {
            start: index,
            end: index,
            text,
        });
    }

    let from = request.from.as_ref().ok_or_else(|| {
        EditError::rejected("input_invalid", "replace requires anchor or from/to")
    })?;
    let to = request.to.as_ref().ok_or_else(|| {
        EditError::rejected("input_invalid", "replace requires anchor or from/to")
    })?;
    let start = anchor_index(anchors, from)?;
    let end = anchor_index(anchors, to)?;
    if start > end {
        return Err(EditError::rejected(
            "input_invalid",
            "from anchor is after to anchor",
        ));
    }
    Ok(Operation::Replace { start, end, text })
}

fn anchor_index(anchors: &[String], anchor: &str) -> Result<usize, EditError> {
    anchors
        .iter()
        .position(|candidate| candidate == anchor)
        .ok_or_else(|| EditError::rejected("anchor_invalid", "anchor not found in current file"))
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

fn split_payload(text: &str) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').map(str::to_string).collect()
    }
}

fn read_jsonl() -> Result<Vec<Value>, String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| error.to_string())?;
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|error| error.to_string()))
        .collect()
}

fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn absolute_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn rejected(req_id: &str, detail: &str, message: impl ToString) -> Value {
    json!({
        "req_id": req_id,
        "status": "rejected",
        "detail": detail,
        "content": {
            "message": message.to_string()
        }
    })
}

fn failed(req_id: &str, message: impl ToString) -> Value {
    json!({
        "req_id": req_id,
        "status": "failed",
        "content": {
            "message": message.to_string()
        }
    })
}

enum Operation {
    Replace {
        start: usize,
        end: usize,
        text: String,
    },
    Insert {
        index: usize,
        text: String,
    },
}

enum EditError {
    Rejected { detail: String, message: String },
    Failed(String),
}

impl EditError {
    fn rejected(detail: &str, message: &str) -> Self {
        Self::Rejected {
            detail: detail.to_string(),
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
        assert_eq!(split_payload(""), Vec::<String>::new());
        assert_eq!(
            split_payload("a\nb"),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}
