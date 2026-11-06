hk-pipeline is a local, self-contained semantic news intelligence pipeline written entirely in Rust. It ingests articles from any news source through a trait-based connector architecture — currently implemented for the GDELT Project, a real-time index of news across 65 languages and 200+ countries, but designed to work with any data source including RSS feeds, NewsAPI, proprietary APIs, or local document collections. Adding a new data source requires only implementing a single Connector trait.

Each article title is converted into a 384-dimensional semantic embedding vector using the BAAI/bge-small-en-v1.5 BERT model running locally via Hugging Face's Candle framework. No cloud API, no Python, no external inference service. All model inference happens on your machine, keeping your data private and your operating costs at zero.

Embeddings are stored alongside article metadata in SurrealDB with URL-based deduplication to prevent redundant records across pipeline runs. Once articles are embedded and stored, the pipeline supports semantic similarity search — given any natural language phrase, it retrieves the most semantically related articles from the database ranked by cosine similarity score. Unlike keyword search, this approach matches on meaning rather than exact word overlap, allowing a query like "Boeing production problems" to surface relevant articles that share no literal words with the query.

The pipeline is built as a multi-crate Rust workspace with clean separation between the connector layer (data ingestion), embedder (ML inference), and sink (storage and retrieval). Each layer is independently swappable — swap the connector for any data source, swap the model for any BERT-compatible architecture, or extend the sink with scheduled runs and topic monitoring as the corpus grows over time.

A drift layer clusters the stored embeddings with k-means and saves a snapshot of each run. Each cluster is matched to its closest cluster from the previous run, and trends are ranked by how much they grew, so new and fast-growing topics rise to the top.

## hk-agent

`hk-agent` lets you ask Claude about the corpus in plain language. Claude can look up what's stored, search it by meaning, pull in more GDELT articles, run trend analysis and save Markdown reports. Article fetching, embedding and storage still happen locally. The agent sends tool results, such as article titles and trend summaries, to the Anthropic API.

```sh
export ANTHROPIC_API_KEY=...        # fish: set -x ANTHROPIC_API_KEY ...
surreal start --bind 127.0.0.1:8000 rocksdb:///path/to/hk-pipeline-db
cargo run --release -p hk-agent -- "What's trending in aerospace safety? Write a report."
cargo run --release -p hk-agent     # interactive session
```

| Tool | What it does |
|------|--------------|
| `corpus_stats` | Counts articles per source, with the oldest and newest insert times |
| `search_articles` | Semantic search over stored titles |
| `ingest_gdelt` | Fetches, embeds and stores new GDELT articles |
| `analyze_trends` | Clusters the corpus and ranks trends by growth since the previous run |
| `write_report` | Saves a Markdown report to `reports/` |

- **Approvals:** reading runs without asking. Writes run without asking unless you pass `--confirm-writes`.
- **Limits:** each message gets at most 25 model calls (`--max-turns`).
- **Transcripts:** every session is saved as JSONL in `agent-runs/`.
- **Model:** defaults to `claude-opus-5-5`; change it with `--model` or `HK_AGENT_MODEL`.

### Using it from Claude Code (MCP)

`hk-mcp` serves the same tools over the [Model Context Protocol](https://modelcontextprotocol.io), so Claude Code, or any other MCP host, can drive the pipeline. The host runs the model, so this works with a Claude Pro or Max login and needs no API key.

```sh
cargo build --release -p hk-agent --bin hk-mcp
claude mcp add hk -- "$PWD/target/release/hk-mcp" \
  --reports-dir "$PWD/reports" --model-dir "$PWD/models/bge-small"
claude   # then ask: "what's trending in aerospace safety? write a report"
```

- **Permissions:** Claude Code asks before each tool call, as it does for any MCP tool. Read tools are marked `readOnlyHint`.
- **`--read-only`:** offers only `corpus_stats` and `search_articles`.
- **Publish-level tools** are never offered over MCP.
- **Protocol:** newline-delimited JSON-RPC over stdio, implemented in [`crates/agent/src/mcp.rs`](crates/agent/src/mcp.rs) with no SDK dependency. It supports `initialize`, `ping`, `tools/list` and `tools/call`.
- If `ANTHROPIC_API_KEY` is set in your shell, Claude Code may use it instead of your subscription. Unset it to stay on the plan.

See [`crates/agent/README.md`](crates/agent/README.md) for all options, how to add a tool, and the tests.
