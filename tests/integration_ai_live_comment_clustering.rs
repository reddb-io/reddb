use reddb::application::{CreateRowInput, EntityUseCases, ExecuteQueryInput, QueryUseCases};
use reddb::json::{from_str, to_string, Map, Value as JsonValue};
use reddb::server::RedDBServer;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

const COMMENTS_PER_TOPIC: usize = 20;
const TOPIC_COUNT: usize = 10;

#[derive(Clone, Copy)]
struct TopicSpec {
    slug: &'static str,
    keyword: &'static str,
}

const TOPICS: [TopicSpec; TOPIC_COUNT] = [
    TopicSpec {
        slug: "billing",
        keyword: "invoice",
    },
    TopicSpec {
        slug: "shipping",
        keyword: "delivery",
    },
    TopicSpec {
        slug: "login",
        keyword: "password",
    },
    TopicSpec {
        slug: "mobile-app",
        keyword: "android",
    },
    TopicSpec {
        slug: "performance",
        keyword: "slow",
    },
    TopicSpec {
        slug: "ux",
        keyword: "screen",
    },
    TopicSpec {
        slug: "notifications",
        keyword: "notification",
    },
    TopicSpec {
        slug: "security",
        keyword: "fraud",
    },
    TopicSpec {
        slug: "support",
        keyword: "agent",
    },
    TopicSpec {
        slug: "refunds",
        keyword: "refund",
    },
];

#[derive(Debug, Clone)]
struct ClusterAssignment {
    cluster_id: i32,
    linked_row_id: u64,
    content: String,
}

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("failed to create in-memory runtime")
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn required_env(key: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("missing required env var: {key}"))
}

fn optional_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn spawn_reddb_http(rt: RedDBRuntime) -> String {
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("http listener should bind");
    let addr = listener
        .local_addr()
        .expect("http listener should expose a local addr");

    thread::spawn(move || {
        let _ = server.serve_on(listener);
    });

    format!("http://{addr}")
}

fn http_post_json(url: &str, payload: &JsonValue) -> JsonValue {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(120))
        .build();
    let body = to_string(payload).expect("payload should serialize");
    let response = agent
        .post(url)
        .set("content-type", "application/json")
        .send_string(&body);

    let body = match response {
        Ok(resp) => resp.into_string().expect("response body should read"),
        Err(ureq::Error::Status(_, resp)) => resp.into_string().expect("error body should read"),
        Err(err) => panic!("http request failed: {url}\nerror: {err}"),
    };
    from_str(&body).expect("response should be valid json")
}

fn json_object(entries: Vec<(&str, JsonValue)>) -> JsonValue {
    let mut object = Map::new();
    for (key, value) in entries {
        object.insert(key.to_string(), value);
    }
    JsonValue::Object(object)
}

fn seed_comments(rt: &RedDBRuntime) {
    let entity = EntityUseCases::new(rt);
    let mut inserted = 0usize;

    for topic in TOPICS {
        for index in 0..COMMENTS_PER_TOPIC {
            let comment = format!("{} {} {}", topic.keyword, topic.slug, index);
            entity
                .create_row(CreateRowInput {
                    collection: "comments".into(),
                    fields: vec![
                        ("comment".into(), Value::Text(comment)),
                        ("author".into(), Value::Text(format!("user-{index}"))),
                        ("category_id".into(), Value::Null),
                    ],
                    metadata: vec![],
                    node_links: vec![],
                    vector_links: vec![],
                })
                .expect("comment insert should succeed");
            inserted += 1;
        }
    }

    assert_eq!(inserted, TOPICS.len() * COMMENTS_PER_TOPIC);
}

fn cluster_assignments(cluster_json: &JsonValue) -> Vec<ClusterAssignment> {
    cluster_json
        .get("assignments")
        .and_then(JsonValue::as_array)
        .expect("cluster response should include assignments")
        .iter()
        .map(|assignment| ClusterAssignment {
            cluster_id: assignment
                .get("cluster_id")
                .and_then(JsonValue::as_i64)
                .map(|value| value as i32)
                .expect("assignment cluster_id"),
            linked_row_id: assignment
                .get("linked_row_id")
                .and_then(JsonValue::as_i64)
                .map(|value| value as u64)
                .expect("assignment linked_row_id"),
            content: assignment
                .get("content")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .expect("assignment content"),
        })
        .collect()
}

fn grouped_cluster_prompts(assignments: &[ClusterAssignment]) -> (Vec<i32>, Vec<String>) {
    let mut by_cluster: BTreeMap<i32, Vec<&ClusterAssignment>> = BTreeMap::new();
    for assignment in assignments {
        by_cluster
            .entry(assignment.cluster_id)
            .or_default()
            .push(assignment);
    }

    let mut cluster_ids = Vec::with_capacity(by_cluster.len());
    let mut prompts = Vec::with_capacity(by_cluster.len());

    for (cluster_id, items) in by_cluster {
        cluster_ids.push(cluster_id);
        let examples = items
            .iter()
            .take(5)
            .map(|assignment| format!("- {}", assignment.content))
            .collect::<Vec<_>>()
            .join("\n");
        prompts.push(format!(
            "Name this cluster of user comments. Return only one short category_id in lowercase kebab-case.\nExamples:\n{examples}"
        ));
    }

    (cluster_ids, prompts)
}

#[test]
#[ignore = "requires live OpenAI credentials and makes real provider calls"]
fn live_comment_clustering_calls_real_models() {
    let _env_lock = env_lock().lock().expect("env lock should be available");

    let api_key = required_env("REDDB_OPENAI_API_KEY");
    let embedding_model = optional_env("REDDB_OPENAI_EMBEDDING_MODEL")
        .unwrap_or_else(|| "text-embedding-3-small".to_string());
    let prompt_model =
        optional_env("REDDB_OPENAI_PROMPT_MODEL").unwrap_or_else(|| "gpt-4.1-mini".to_string());

    let rt = rt();
    seed_comments(&rt);
    let http_base = spawn_reddb_http(rt.clone());
    let query = QueryUseCases::new(&rt);

    let mut embedding_entries = vec![
        ("provider", JsonValue::String("openai".to_string())),
        ("model", JsonValue::String(embedding_model.clone())),
        (
            "source_query",
            JsonValue::String(
                "SELECT red_entity_id, comment FROM comments ORDER BY red_entity_id ASC LIMIT 200"
                    .to_string(),
            ),
        ),
        ("source_mode", JsonValue::String("row".to_string())),
        ("source_field", JsonValue::String("comment".to_string())),
        (
            "source_collection",
            JsonValue::String("comments".to_string()),
        ),
        ("max_inputs", JsonValue::Number(200.0)),
        (
            "save",
            json_object(vec![
                (
                    "collection",
                    JsonValue::String("comment_embeddings".to_string()),
                ),
                ("include_content", JsonValue::Bool(true)),
            ]),
        ),
    ];
    if let Some(api_base) = optional_env("REDDB_OPENAI_API_BASE") {
        embedding_entries.push(("api_base", JsonValue::String(api_base)));
    }
    embedding_entries.push(("api_key", JsonValue::String(api_key.clone())));

    let embeddings = http_post_json(
        &format!("{http_base}/ai/embeddings"),
        &json_object(embedding_entries),
    );
    assert_eq!(
        embeddings.get("ok").and_then(JsonValue::as_bool),
        Some(true),
        "unexpected embeddings response: {}",
        embeddings
    );
    assert_eq!(
        embeddings.get("count").and_then(JsonValue::as_i64),
        Some(200),
        "unexpected embedding count: {}",
        embeddings
    );
    assert!(
        embeddings
            .get("prompt_tokens")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0)
            > 0,
        "live embedding call should report usage: {}",
        embeddings
    );

    let clusters = http_post_json(
        &format!("{http_base}/vectors/cluster"),
        &json_object(vec![
            (
                "collection",
                JsonValue::String("comment_embeddings".to_string()),
            ),
            ("algorithm", JsonValue::String("kmeans".to_string())),
            ("k", JsonValue::Number(10.0)),
            ("max_iterations", JsonValue::Number(50.0)),
        ]),
    );
    assert_eq!(
        clusters.get("ok").and_then(JsonValue::as_bool),
        Some(true),
        "unexpected cluster response: {}",
        clusters
    );

    let assignments = cluster_assignments(&clusters);
    assert_eq!(assignments.len(), 200);

    let (cluster_ids, prompts) = grouped_cluster_prompts(&assignments);
    assert_eq!(cluster_ids.len(), TOPIC_COUNT);

    let mut prompt_entries = vec![
        ("provider", JsonValue::String("openai".to_string())),
        ("model", JsonValue::String(prompt_model.clone())),
        (
            "prompts",
            JsonValue::Array(prompts.into_iter().map(JsonValue::String).collect()),
        ),
        ("api_key", JsonValue::String(api_key)),
    ];
    if let Some(api_base) = optional_env("REDDB_OPENAI_API_BASE") {
        prompt_entries.push(("api_base", JsonValue::String(api_base)));
    }

    let labels = http_post_json(
        &format!("{http_base}/ai/prompt"),
        &json_object(prompt_entries),
    );
    assert_eq!(
        labels.get("ok").and_then(JsonValue::as_bool),
        Some(true),
        "unexpected prompt response: {}",
        labels
    );

    let outputs = labels
        .get("outputs")
        .and_then(JsonValue::as_array)
        .expect("prompt outputs");
    assert_eq!(outputs.len(), TOPIC_COUNT);
    assert!(
        labels
            .get("completion_tokens")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0)
            > 0,
        "live prompt call should report usage: {}",
        labels
    );

    let mut category_by_cluster = BTreeMap::new();
    for (cluster_id, output) in cluster_ids.iter().zip(outputs.iter()) {
        let category_id = output
            .get("text")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .expect("prompt output text");
        category_by_cluster.insert(*cluster_id, category_id);
    }

    for assignment in &assignments {
        let category_id = category_by_cluster
            .get(&assignment.cluster_id)
            .expect("category for cluster");
        query
            .execute(ExecuteQueryInput {
                query: format!(
                    "UPDATE comments SET category_id = '{}' WHERE red_entity_id = {}",
                    category_id.replace('\'', "''"),
                    assignment.linked_row_id
                ),
            })
            .unwrap_or_else(|err| panic!("category writeback should succeed: {err:?}"));
    }

    let grouped = query
        .execute(ExecuteQueryInput {
            query: "SELECT category_id, count(*) AS total FROM comments GROUP BY category_id ORDER BY category_id ASC".into(),
        })
        .expect("grouped category query should succeed");

    let total_rows: i64 = grouped
        .result
        .records
        .iter()
        .map(|record| match record.get("total") {
            Some(Value::Integer(value)) => *value,
            Some(Value::UnsignedInteger(value)) => *value as i64,
            other => panic!("expected total count, got {other:?}"),
        })
        .sum();
    assert_eq!(total_rows, 200);

    let categories = grouped
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("category_id") {
            Some(Value::Text(value)) if !value.trim().is_empty() => Some(value.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        !categories.is_empty(),
        "live flow should produce at least one category"
    );

    eprintln!("embedding_model={embedding_model}");
    eprintln!("prompt_model={prompt_model}");
    eprintln!("categories={categories:?}");
}
