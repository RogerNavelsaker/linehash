#!/usr/bin/env bash
source "$(dirname "$0")/common.sh"

main() {
    info "Building release binary..."
    cargo build --release

    info "Generating large test file (20,000 lines)..."
    python3 -c '
with open("large_test.rs", "w") as f:
    f.write("fn main() {\n")
    for i in range(10000):
        f.write(f"    let var_{i} = {i};\n")
        f.write(f"    println!(\"{i}\");\n")
    f.write("}\n")
'

    info "Creating Bun hash reference script..."
    cat << 'BUNEOF' > bun_hash.ts
import { readFileSync } from "fs";
function main() {
  const content = readFileSync(process.argv[2], "utf-8");
  const lines = content.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i].trim();
    if (trimmed !== "") {
        (Number(Bun.hash(trimmed) % 256n)).toString(16).padStart(2, '0');
    }
  }
}
main();
BUNEOF

    info "Running Hyperfine Read Benchmark..."
    ~/.cargo/bin/hyperfine --warmup 3 \
      "echo '{\"path\":\"large_test.rs\"}' | target/release/linehash read > /dev/null" \
      "bun bun_hash.ts large_test.rs > /dev/null"

    info "Preparing Apply Benchmark payload..."
    target/release/linehash read large_test.rs > read.jsonl
    python3 -c '
import json
read = json.load(open("read.jsonl"))
lines = read["content"]["lines"]
anchors = {line["line"]: line["anchor"] for line in lines}
with open("edit.jsonl", "w") as f:
    f.write(json.dumps({"path": "large_test.rs", "op": "replace", "anchor": anchors[499], "text": "    println!(\"248 - EDITED\");"}) + "\n")
    f.write(json.dumps({"path": "large_test.rs", "op": "replace", "from": anchors[502], "to": anchors[503], "text": "    let var_250 = 250000;\n    println!(\"250000\");"}) + "\n")
'

    cp large_test.rs large_test.rs.bak
    info "Running Hyperfine Apply Benchmark..."
    ~/.cargo/bin/hyperfine --warmup 3 \
      --prepare "cp large_test.rs.bak large_test.rs" \
      "cat edit.jsonl | target/release/linehash edit > /dev/null"

    info "Cleaning up..."
    rm generate_test.py bun_hash.ts large_test.rs large_test.rs.bak read.jsonl edit.jsonl
    info "Benchmarks complete."
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
