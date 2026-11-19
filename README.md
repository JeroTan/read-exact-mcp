# exact-doc-reader

A local Rust MCP server with one tool, `read_exact_files`. Pass an ordered array of paths relative to `PROJECT_ROOT`. The tool returns one MCP text block containing JSON. Each entry has the requested `path`, the original UTF-8 `content`, its `byteLength`, and its lowercase `sha256`. JSON decoding restores the original text, including CRLF, tabs, and trailing newlines.

The server reads regular files to EOF and sets `complete: true` after every read succeeds. A missing, unreadable, or invalid UTF-8 file produces a tool error for the entire batch. Paths are opened relative to a held project directory. Relative symlinks can resolve inside it. Absolute paths, absolute symlink targets, and traversal outside the root are rejected. The server communicates over stdio and imposes no file-size or token limit.

## Build

From this directory, using stable Rust:

```sh
cargo test
cargo build --release
```

Codex launches `target/release/exact-doc-reader` directly. The following manual checks use a macOS or Linux shell.

## Check the server with MCP Inspector

Create a temporary project and two files. Run these commands in the same shell. The Inspector command uses `npx` to launch the external test client. The server itself needs only the compiled executable. [MCP Inspector guide](https://modelcontextprotocol.io/docs/tools/inspector)

```sh
reader="$PWD/target/release/exact-doc-reader"
project=$(mktemp -d)
printf 'START %s\r\n\tSnow 雪  \nMIDDLE %s\nEND %s\n' \
  "$(uuidgen)" "$(uuidgen)" "$(uuidgen)" > "$project/test-document.txt"
printf 'second file without a final newline' > "$project/second.txt"

inspect() {
  npx -y @modelcontextprotocol/inspector@2.16.0 --cli "$reader" -e "PROJECT_ROOT=$project" "$@"
}
inspect --method tools/list
```

The list should contain only `read_exact_files`, with one required parameter, `files`. Call it with one file, then with two:

```sh
read_files() {
  inspect --method tools/call --tool-name read_exact_files --tool-args-json "$1"
}
read_files '{"files":["test-document.txt"]}'
result=$(mktemp)
read_files '{"files":["second.txt","test-document.txt"]}' > "$result"
```

Verify the decoded text, order, and metadata independently with `jq`, `cmp`, `wc`, and `shasum`. The check prints `Verified both files` on success.

```sh
(
  set -e
  jq -e '(.isError == false) and (.content[0].text | fromjson |
    .complete and ([.files[].path] == ["second.txt","test-document.txt"]))' "$result"
  for name in second.txt test-document.txt; do
    original="$project/$name"
    jq -ej --arg name "$name" '.content[0].text | fromjson | .files[] |
      select(.path == $name) | .content' "$result" | cmp - "$original"
    jq -e --arg name "$name" --argjson size "$(wc -c < "$original")" \
      --arg hash "$(shasum -a 256 "$original" | cut -d' ' -f1)" \
      '.content[0].text | fromjson | .files[] | select(.path == $name) |
       .byteLength == $size and .sha256 == $hash' "$result"
  done
  echo 'Verified both files'
)
```

## Connect Codex

Add this entry to `~/.codex/config.toml`. Replace the two example paths with the absolute executable path and the directory you want to read. For the manual check, use the values printed by `echo "$reader"` and `echo "$project"`. To use another project, change `PROJECT_ROOT` and start a new session. [Codex MCP configuration](https://developers.openai.com/codex/mcp)

```toml
[mcp_servers.exact_doc_reader]
command = "/absolute/path/to/exact-doc-reader/target/release/exact-doc-reader"

[mcp_servers.exact_doc_reader.env]
PROJECT_ROOT = "/absolute/path/to/my-project"
```

Run `codex mcp list` to confirm registration. In a fresh Codex CLI session, run `/mcp` and look for `exact_doc_reader` and its tool. To confirm invocation and delivery, ask:

> Use only read_exact_files with {"files":["test-document.txt"]}. Return the entire content as one JSON string, preserving all whitespace. Use only the tool response.

Save the model's JSON string to a file, then run `jq -j . reply.json | cmp - "$project/test-document.txt"`. A successful comparison proves the model reproduced every byte for that call.

## Check a larger result

Inspector's byte comparison measures **server output**. Codex can limit **client-delivered output** before it reaches the model. The server's completion flag and hash describe what it read. To probe client delivery, put unpredictable markers at the start, middle, and end of a larger file:

```sh
awk -v a="$(uuidgen)" -v b="$(uuidgen)" -v c="$(uuidgen)" 'BEGIN {
  print "START", a
  for (i=0; i<12000; i++) print "Text sample line", i
  print "MIDDLE", b
  for (i=0; i<12000; i++) print "More text line", i
  print "END", c
}' > "$project/large.txt"
read_files '{"files":["large.txt"]}' > "$result"
jq -ej '.content[0].text | fromjson | .files[0].content' "$result" | cmp - "$project/large.txt"
```

Ask Codex to call the tool once for `large.txt` and quote all three marker values it sees, using only the unmodified tool result. Compare its answer with `awk '/^(START|MIDDLE|END) /' "$project/large.txt"`. Matching markers confirm those positions. A full byte comparison is needed to establish exact delivery.

In a Codex CLI 0.166.0 check, the model reproduced the 141-byte fixture exactly. For the 505,908-byte fixture it reported the first and last markers and marked the middle one unavailable. Inspector returned both originals byte-for-byte.
