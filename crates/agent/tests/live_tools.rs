//! Runs the agent loop with a scripted model against a real SurrealDB and the
//! real embedding model. Skipped unless HK_AGENT_TEST_SURREAL is set, e.g.:
//!
//!   surreal start memory --bind 127.0.0.1:8123 --user root --pass root
//!   HK_AGENT_TEST_SURREAL=127.0.0.1:8123 cargo test -p hk-agent --test live_tools

use std::path::PathBuf;
use std::sync::Arc;

use embedder::Embedder;
use hk_agent::harness::{Agent, Config, Outcome, ScriptedLlm};
use hk_agent::llm::Response;
use hk_agent::pipeline_tools::{self, PipelineContext};
use hk_agent::tool::{DenyApprover, Policy, ToolRegistry};
use serde_json::{json, Value};
use sink::{ArticleRecord, SurrealSink};

fn call(id: &str, name: &str, input: Value) -> Response {
    serde_json::from_value(json!({
        "content": [{ "type": "tool_use", "id": id, "name": name, "input": input }],
        "stop_reason": "tool_use"
    }))
    .unwrap()
}

fn tool_output(llm: &ScriptedLlm, request: usize) -> Value {
    let requests = llm.requests.lock().unwrap();
    let block = &requests[request].messages.last().unwrap().content[0];
    assert_eq!(block["is_error"], false, "tool failed: {}", block["content"]);
    serde_json::from_str(block["content"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn tools_work_against_real_services() {
    let Ok(url) = std::env::var("HK_AGENT_TEST_SURREAL") else {
        eprintln!("skipped: HK_AGENT_TEST_SURREAL not set");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let embedder = Embedder::new(&root.join("models/bge-small")).unwrap();
    let sink = SurrealSink::new(&url, "root", "root").await.unwrap();
    sink.setup().await.unwrap();

    let titles = [
        "Boeing 737 MAX production halted after door plug inspection",
        "FAA audits Boeing assembly line quality control",
        "Spirit AeroSystems fuselage defects delay deliveries",
        "Electric air taxi startup completes first piloted flight",
        "Hydrogen-powered regional aircraft passes ground tests",
        "Sustainable aviation fuel output doubles at refinery",
        "Pentagon awards fighter jet engine contract",
        "Army selects new attack helicopter supplier",
        "Missile defense radar contract goes to Raytheon",
    ];
    for (i, t) in titles.iter().enumerate() {
        sink.insert_if_new(ArticleRecord {
            title: t.to_string(),
            url: Some(format!("https://example.test/{i}")),
            published: None,
            source: "test".into(),
            embedding: embedder.embed(t).unwrap(),
            inserted_at: format!("2026-11-05T00:00:0{i}Z"),
        })
        .await
        .unwrap();
    }

    let reports = std::env::temp_dir().join(format!("hk-agent-test-{}", uuid::Uuid::new_v4()));
    let ctx = Arc::new(PipelineContext::new(embedder, sink, reports.clone(), 0.85));
    let mut tools = ToolRegistry::default();
    pipeline_tools::register(&mut tools, ctx);

    let llm = Arc::new(ScriptedLlm::new(vec![
        call("t1", "corpus_stats", json!({})),
        call("t2", "search_articles", json!({ "query": "electric aircraft", "limit": 3 })),
        call("t3", "analyze_trends", json!({ "clusters": 3, "save_snapshot": true })),
        call("t4", "write_report", json!({ "filename": "test", "markdown": "# ok" })),
        serde_json::from_value(json!({
            "content": [{ "type": "text", "text": "report written" }],
            "stop_reason": "end_turn"
        }))
        .unwrap(),
    ]));
    let mut agent = Agent::new(
        llm.clone(),
        tools,
        Arc::new(DenyApprover),
        Config {
            model: "test".into(),
            max_tokens: 100,
            max_turns: 10,
            system: "sys".into(),
            policy: Policy { auto_approve_writes: true },
        },
    );
    let reply = agent.send("analyze").await.unwrap();
    assert_eq!(reply.outcome, Outcome::Done);

    let stats = tool_output(&llm, 1);
    assert!(stats["total_articles"].as_u64().unwrap() >= 9);
    assert_eq!(stats["oldest_inserted_at"], "2026-11-05T00:00:00Z");
    assert_eq!(stats["missing_inserted_at"], 0);

    let search = tool_output(&llm, 2);
    assert_eq!(search["results"].as_array().unwrap().len(), 3);
    let top = search["results"][0]["title"].as_str().unwrap();
    eprintln!("top search hit: {top}");
    assert!(top.contains("air taxi") || top.contains("Hydrogen") || top.contains("aviation fuel"));

    let trends = tool_output(&llm, 3);
    assert_eq!(trends["clusters"].as_array().unwrap().len(), 3);
    assert_eq!(trends["trends"][0]["rank"], 1);
    eprintln!("clusters: {}", trends["clusters"]);

    let report = tool_output(&llm, 4);
    assert_eq!(std::fs::read_to_string(reports.join("test.md")).unwrap(), "# ok");
    assert!(report["path"].as_str().unwrap().ends_with("test.md"));
    std::fs::remove_dir_all(&reports).unwrap();
}
