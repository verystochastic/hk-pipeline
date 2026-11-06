# hk-agent

Ask Claude questions about the hk-pipeline corpus. It can ingest more articles, run trend analysis and write Markdown reports.

## Run

From the workspace root, with SurrealDB running:

```sh
export ANTHROPIC_API_KEY=...
cargo run --release -p hk-agent -- "What's trending in aerospace safety? Write a report."
cargo run --release -p hk-agent            # interactive session
```

## Tools

| Tool | Risk | What it does |
|------|------|--------------|
| `corpus_stats` | read | Counts articles per source, with the oldest and newest insert times |
| `search_articles` | read | Semantic search over titles |
| `ingest_gdelt` | write | Fetches, embeds and stores new GDELT articles |
| `analyze_trends` | write | Clusters the corpus, ranks trends against the previous run and saves snapshots |
| `write_report` | write | Saves Markdown to `--reports-dir`, plain file names only |

## Approvals

- **Read:** runs without asking.
- **Write:** runs without asking unless you pass `--confirm-writes`.
- **Publish:** for irreversible actions, which no current tool has. You must type `publish` each time.

With no terminal attached, every approval request is refused.

## Options

| Flag / env | Default |
|------------|---------|
| `--model` / `HK_AGENT_MODEL` | `claude-opus-5-5` |
| `--max-turns` | 25 model calls per message |
| `--transcript-dir` | `agent-runs/`, one JSONL file per session |
| `--reports-dir` | `reports/` |

SurrealDB and model directory flags match `hk-pipeline`.

## Layout

- `src/llm.rs`: Messages API client, with retries and prompt caching for the system prompt and tool list
- `src/tool.rs`: `Tool` trait, risk levels, approval policy
- `src/harness.rs`: the agent loop, tool-result truncation, transcripts, and `ScriptedLlm` for tests
- `src/pipeline_tools.rs`: tools that wrap the pipeline crates

To add a tool, implement `Tool` (or use the `tool!` macro in `pipeline_tools.rs`) and register it in `register()`.

## Tests

```sh
cargo test -p hk-agent
# against real SurrealDB and the embedding model:
surreal start memory --bind 127.0.0.1:8123 --user root --pass root
HK_AGENT_TEST_SURREAL=127.0.0.1:8123 cargo test -p hk-agent --test live_tools
```
