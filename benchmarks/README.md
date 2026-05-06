# linehash (le) Benchmarks

This directory contains scripts to test and benchmark the performance and reliability of `linehash` (`le`).

## 1. Hyperfine Performance Benchmark

Measures the raw execution speed (latency) of the Rust CLI vs a comparable native script in Bun (simulating an MCP server execution).

```bash
chmod +x hyperfine_bench.sh
./hyperfine_bench.sh
```
This generates a 20,000 line synthetic file and runs `hyperfine` to compare reading and applying times.

## 2. LLM Agent Hit Rate A/B Test

Tests how reliably an LLM agent (Gemini, Claude Code, Codex) can perform edits with and without `linehash` (`le`).

**Prerequisites:**
You need a directory containing test cases (e.g., `dataset/test_1`). Each test case must be a valid Rust project (or another language you are testing) containing a `prompt.txt` that describes the bug to fix, and a test suite that fails initially but will pass if the LLM makes the right edit.

**Running the Control Group (Native Editing Tools):**
```bash
chmod +x hit_rate_bench.sh
./hit_rate_bench.sh --agent "gemini --yolo" --dataset ./dataset
```

**Running the Experimental Group (With linehash skill):**
Make sure the `linehash` (`le`) skill is loaded in your environment, then run:
```bash
./hit_rate_bench.sh --agent "gemini --yolo" --dataset ./dataset --use-skill
```

Compare the hit rate (%) between the two runs to measure the reduction in hallucinated or malformed edits!

## 3. Results (Hyperfine Benchmarks)

These benchmarks were run on a synthetic 20,000-line Rust file (`large_test.rs`) within the default `flox` environment. They compare the native Rust `linehash` (`le`) against a simulated MCP execution (running via `bun` to approximate a fast Node/JS environment without full JSON-RPC IPC overhead).

### Read Benchmark (Hashing 20,000 lines)
```text
Benchmark 1: echo '{"path":"large_test.rs"}' | target/release/linehash read > /dev/null
  Time (mean ± σ):       4.9 ms ±   1.0 ms    [User: 3.0 ms, System: 1.8 ms]
  Range (min … max):     3.2 ms …   9.1 ms    381 runs

Benchmark 2: bun bun_hash.ts large_test.rs > /dev/null
  Time (mean ± σ):      19.7 ms ±   1.6 ms    [User: 14.3 ms, System: 7.6 ms]
  Range (min … max):    17.1 ms …  34.9 ms    153 runs

Summary
  echo '{"path":"large_test.rs"}' | target/release/linehash read > /dev/null ran
    4.03 ± 0.92 times faster than bun bun_hash.ts large_test.rs > /dev/null
```
**Takeaway:** The native Rust implementation calculates cryptographic hashes for 20,000 lines natively in roughly **4.9 milliseconds**—significantly faster than even the raw script execution time of Bun.

### Apply Benchmark (JSON verification and disk write)
Applying a multi-line JSON replacement directly into the middle of the 20,000-line file natively:

```text
Benchmark 1: cat edit.jsonl | target/release/linehash edit > /dev/null
  Time (mean ± σ):       4.2 ms ±   0.7 ms    [User: 1.7 ms, System: 2.1 ms]
  Range (min … max):     2.8 ms …   7.1 ms    361 runs
```
**Takeaway:** A full end-to-end cycle (parsing JSON, reading the 20,000 line file, verifying the cryptographic hashes of the target lines, applying the edits, and overwriting the file) completed in **4.2 milliseconds**, entirely bypassing the overhead of standard MCP JSON-RPC protocol layers.

---
*Note: To run the LLM Agent A/B hit rate tests or the Chaos Monkey benchmark, you must supply an external dataset of test cases and ensure the relevant agent CLI is authenticated and in your PATH.*
