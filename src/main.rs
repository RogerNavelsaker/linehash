use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read};
use std::process;
use xxhash_rust::xxh32::xxh32;

#[derive(Parser)]
#[command(author, version, about = "LLM compact hashmap line editor")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            command: Commands::Skill,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Read file to hashlined JSON array
    Read { file: String },
    /// Apply edits from JSON array on stdin
    Apply { file: String },
    /// Generate skill markdown with instructions and schema
    Skill,
}

#[derive(Serialize)]
struct LineOutput {
    l: usize,
    h: String,
    c: String,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "op")]
enum Edit {
    #[serde(rename = "r")]
    Replace { l: usize, h: String, c: String },
    #[serde(rename = "mr")]
    ReplaceMulti {
        s: usize,
        e: usize,
        h: Vec<String>,
        c: Vec<String>,
    },
    #[serde(rename = "i")]
    Insert { l: usize, h: String, c: Vec<String> },
    #[serde(rename = "d")]
    Delete { s: usize, e: usize, h: Vec<String> },
}

impl Edit {
    fn start_line(&self) -> usize {
        match self {
            Edit::Replace { l, .. } => *l,
            Edit::ReplaceMulti { s, .. } => *s,
            Edit::Insert { l, .. } => *l,
            Edit::Delete { s, .. } => *s,
        }
    }
}

const JSON_SCHEMA: &str = r#"{
  "type": "array",
  "items": {
    "type": "object",
    "oneOf": [
      {
        "properties": {
          "op": { "const": "r" },
          "l": { "type": "integer", "description": "Line number to replace (1-based)" },
          "h": { "type": "string", "description": "2-char hex hash of the original line" },
          "c": { "type": "string", "description": "New content" }
        },
        "required": ["op", "l", "h", "c"]
      },
      {
        "properties": {
          "op": { "const": "mr" },
          "s": { "type": "integer", "description": "Start line number (1-based)" },
          "e": { "type": "integer", "description": "End line number (inclusive)" },
          "h": { "type": "array", "items": { "type": "string" }, "description": "Array of hashes for each line in range" },
          "c": { "type": "array", "items": { "type": "string" }, "description": "Array of new lines" }
        },
        "required": ["op", "s", "e", "h", "c"]
      },
      {
        "properties": {
          "op": { "const": "i" },
          "l": { "type": "integer", "description": "Line number to insert after (use 0 for BOF)" },
          "h": { "type": "string", "description": "Hash of line 'l' (use '00' for BOF)" },
          "c": { "type": "array", "items": { "type": "string" }, "description": "Array of new lines to insert" }
        },
        "required": ["op", "l", "h", "c"]
      },
      {
        "properties": {
          "op": { "const": "d" },
          "s": { "type": "integer", "description": "Start line number to delete (1-based)" },
          "e": { "type": "integer", "description": "End line number to delete (inclusive)" },
          "h": { "type": "array", "items": { "type": "string" }, "description": "Array of hashes for each line to delete" }
        },
        "required": ["op", "s", "e", "h"]
      }
    ]
  }
}"#;

const SKILL_MARKDOWN: &str = r#"---
name: le
description: A high-performance, compact hashmap line editor for safe and deterministic file modifications.
---

# Linehash Edit (le) Protocol

You are equipped with `le`, a native, zero-latency CLI for line-editing. This tool prevents hallucinations and "lost updates" by requiring cryptographic hashes of the lines you intend to edit.

## Reading Files
To view a file, run:
`le read <filepath>`

The output is a JSON array of objects representing lines:
`[{"l": 1, "h": "2f", "c": "fn main() {"}, ...]`
- `l`: Line number (1-based).
- `h`: 2-character hex hash (ignoring leading/trailing whitespace).
- `c`: The line's content.

## Editing Files
To apply edits, pipe a compact JSON array of edit operations into `le apply <filepath>`.

Supported operations (`op`):
1. **Single-line Replace** (`r`):
   `[{"op": "r", "l": 1, "h": "2f", "c": "fn main(args) {"}]`
2. **Multi-line Replace** (`mr`):
   `[{"op": "mr", "s": 2, "e": 3, "h": ["a1", "b2"], "c": ["let a = 1;", "let b = 2;"]}]`
3. **Insert After** (`i`):
   `[{"op": "i", "l": 10, "h": "d4", "c": ["new line 1", "new line 2"]}]`
   *(To insert at the beginning of the file, use `l: 0` and `h: "00"`)*
4. **Delete Block** (`d`):
   `[{"op": "d", "s": 5, "e": 6, "h": ["e5", "f6"]}]`

### Critical Rules:
1. You MUST provide the exact hash (`h`) for the line(s) being targeted. If the file has changed and the hash doesn't match, the entire batch of edits will be rejected.
2. For multi-line (`mr`) and delete (`d`) operations, the length of the hash array `h` MUST match the number of lines from `s` to `e` inclusive.
3. Edits are processed atomically. Do not send malformed JSON.
4. Output your edits cleanly to `stdin`. For example:
   `echo '[{"op":"r","l":5,"h":"1a","c":"let x = 42;"}]' | le apply src/main.rs`
"#;

fn compute_hash(line: &str) -> String {
    let trimmed = line.trim();
    let h = if trimmed.is_empty() {
        0
    } else {
        xxh32(trimmed.as_bytes(), 0) % 256
    };
    format!("{:02x}", h)
}

fn error_exit(msg: &str, include_schema: bool) -> ! {
    if include_schema {
        let schema: serde_json::Value =
            serde_json::from_str(JSON_SCHEMA).unwrap_or(serde_json::Value::Null);
        eprintln!("{}", serde_json::json!({ "error": msg, "schema": schema }));
    } else {
        eprintln!("{}", serde_json::json!({ "error": msg }));
    }
    process::exit(1);
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Skill => {
            println!("{}", SKILL_MARKDOWN);
        }
        Commands::Read { file } => {
            let content = match fs::read_to_string(&file) {
                Ok(c) => c,
                Err(e) => error_exit(&format!("Failed to read command target file {}: {}. Check permissions or path.", file, e), false),
            };
            let mut out = Vec::new();
            for (i, line) in content.lines().enumerate() {
                out.push(LineOutput {
                    l: i + 1,
                    h: compute_hash(line),
                    c: line.to_string(),
                });
            }
            println!("{}", serde_json::to_string(&out).unwrap());
        }
        Commands::Apply { file } => {
            let mut input = String::new();
            io::stdin()
                .read_to_string(&mut input)
                .unwrap_or_else(|_| error_exit("Failed to read stdin", false));

            let mut edits: Vec<Edit> = match serde_json::from_str(&input) {
                Ok(e) => e,
                Err(e) => error_exit(&format!("Invalid JSON payload: {}", e), true),
            };

            edits.sort_by(|a, b| b.start_line().cmp(&a.start_line()));
            let content = match fs::read_to_string(&file) {
                Ok(c) => c,
                Err(e) => error_exit(&format!("Failed to read {}: {}", file, e), false),
            };
            let has_trailing_newline = content.ends_with('\n');
            let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
            if has_trailing_newline && !lines.is_empty() {
                lines.pop();
            }

            for edit in edits {
                match edit {
                    Edit::Replace { l, h, c } => {
                        let idx = l - 1;
                        if idx >= lines.len() {
                            error_exit(&format!("Line {} out of bounds", l), true);
                        }
                        let expected_h = compute_hash(&lines[idx]);
                        if expected_h != h {
                            error_exit(
                                &format!(
                                    "Hash mismatch at line {}. Expected {}, got {}",
                                    l, expected_h, h
                                ),
                                false,
                            );
                        }
                        lines[idx] = c;
                    }
                    Edit::ReplaceMulti { s, e, h, c } => {
                        let start_idx = s - 1;
                        let end_idx = e - 1;
                        if end_idx >= lines.len() {
                            error_exit(&format!("Line {} out of bounds", e), true);
                        }
                        if h.len() != (e - s + 1) {
                            error_exit("Hash array length must match line range", true);
                        }
                        for (i, hash) in h.iter().enumerate() {
                            let expected_h = compute_hash(&lines[start_idx + i]);
                            if expected_h != *hash {
                                error_exit(
                                    &format!(
                                        "Hash mismatch at line {}. Expected {}, got {}",
                                        s + i,
                                        expected_h,
                                        hash
                                    ),
                                    false,
                                );
                            }
                        }
                        lines.splice(start_idx..=end_idx, c);
                    }
                    Edit::Insert { l, h, c } => {
                        if l == 0 {
                            if h != "00" {
                                error_exit("BOF insert expected hash 00", false);
                            }
                            for (i, new_line) in c.into_iter().enumerate() {
                                lines.insert(i, new_line);
                            }
                        } else {
                            let idx = l - 1;
                            if idx >= lines.len() {
                                error_exit(&format!("Line {} out of bounds", l), true);
                            }
                            let expected_h = compute_hash(&lines[idx]);
                            if expected_h != h {
                                error_exit(
                                    &format!(
                                        "Hash mismatch at line {}. Expected {}, got {}",
                                        l, expected_h, h
                                    ),
                                    false,
                                );
                            }
                            for (i, new_line) in c.into_iter().enumerate() {
                                lines.insert(idx + 1 + i, new_line);
                            }
                        }
                    }
                    Edit::Delete { s, e, h } => {
                        let start_idx = s - 1;
                        let end_idx = e - 1;
                        if end_idx >= lines.len() {
                            error_exit(&format!("Line {} out of bounds", e), true);
                        }
                        if h.len() != (e - s + 1) {
                            error_exit("Hash array length must match line range", true);
                        }
                        for (i, hash) in h.iter().enumerate() {
                            let expected_h = compute_hash(&lines[start_idx + i]);
                            if expected_h != *hash {
                                error_exit(
                                    &format!(
                                        "Hash mismatch at line {}. Expected {}, got {}",
                                        s + i,
                                        expected_h,
                                        hash
                                    ),
                                    false,
                                );
                            }
                        }
                        lines.drain(start_idx..=end_idx);
                    }
                }
            }
            let mut output = lines.join("\n");
            if has_trailing_newline {
                output.push('\n');
            }
            if let Err(e) = fs::write(&file, output) {
                error_exit(&format!("Failed to write to file: {}", e), false);
            }
            println!("{}", serde_json::json!({"status": "success"}));
        }
    }
}
