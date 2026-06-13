use reddb::application::{CreateRowInput, EntityUseCases, ExecuteQueryInput, QueryUseCases};
use reddb::json::{from_str, to_string, Map, Value as JsonValue};
use reddb::server::RedDBServer;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

const COMMENTS_PER_TOPIC: usize = 20;
const TOPIC_COUNT: usize = 10;
const EXPECTED_AI_REQUESTS: usize = 1 + TOPIC_COUNT;

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

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(vars: &[(&'static str, String)]) -> Self {
        let mut saved = Vec::new();
        for (key, value) in vars {
            saved.push((*key, std::env::var(key).ok()));
            unsafe {
                std::env::set_var(key, value);
            }
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => unsafe {
                    std::env::set_var(key, value);
                },
                None => unsafe {
                    std::env::remove_var(key);
                },
            }
        }
    }
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
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into();
    let body = to_string(payload).expect("payload should serialize");
    let response = agent
        .post(url)
        .header("content-type", "application/json")
        .send(body);

    let body = match response {
        Ok(mut resp) => resp
            .body_mut()
            .read_to_string()
            .expect("response body should read"),
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
                        ("comment".into(), Value::text(comment)),
                        ("author".into(), Value::text(format!("user-{index}"))),
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

fn read_http_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream
            .read(&mut chunk)
            .expect("request read should succeed");
        if read == 0 {
            panic!("unexpected EOF while reading request headers");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos + 4;
        }
    };

    let header = String::from_utf8(buffer[..header_end].to_vec()).expect("header utf8");
    let request_line = header.lines().next().unwrap_or_default();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let content_length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    while buffer.len() < header_end + content_length {
        let read = stream
            .read(&mut chunk)
            .expect("request body read should succeed");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    (
        path,
        buffer[header_end..header_end + content_length].to_vec(),
    )
}

fn write_http_json(stream: &mut TcpStream, body: &JsonValue) {
    let body = to_string(body).expect("json body should serialize");
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(response.as_bytes())
        .expect("response write should succeed");
    stream.flush().expect("response flush should succeed");
}

fn topic_for_text(text: &str) -> TopicSpec {
    let lower = text.to_ascii_lowercase();
    TOPICS
        .iter()
        .copied()
        .find(|topic| lower.contains(topic.keyword))
        .unwrap_or_else(|| panic!("unknown topic in text: {text}"))
}

fn embedding_response(body: &[u8]) -> JsonValue {
    let payload: JsonValue =
        from_str(&String::from_utf8_lossy(body)).expect("embedding payload json");
    let inputs: Vec<String> = match payload.get("input") {
        Some(JsonValue::String(value)) => vec![value.clone()],
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .expect("embedding input should be text")
            })
            .collect(),
        other => panic!("unexpected embedding input payload: {other:?}"),
    };

    let data = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            let topic = topic_for_text(input);
            let mut embedding = vec![0.0f64; TOPIC_COUNT];
            let topic_index = TOPICS
                .iter()
                .position(|candidate| candidate.slug == topic.slug)
                .expect("topic index");
            embedding[topic_index] = 1.0;

            let mut item = Map::new();
            item.insert(
                "object".to_string(),
                JsonValue::String("embedding".to_string()),
            );
            item.insert("index".to_string(), JsonValue::Number(index as f64));
            item.insert(
                "embedding".to_string(),
                JsonValue::Array(
                    embedding
                        .into_iter()
                        .map(JsonValue::Number)
                        .collect::<Vec<_>>(),
                ),
            );
            JsonValue::Object(item)
        })
        .collect::<Vec<_>>();

    let token_count = inputs.len() as f64;
    json_object(vec![
        ("object", JsonValue::String("list".to_string())),
        ("data", JsonValue::Array(data)),
        ("model", JsonValue::String("mock-embed".to_string())),
        (
            "usage",
            json_object(vec![
                ("prompt_tokens", JsonValue::Number(token_count)),
                ("total_tokens", JsonValue::Number(token_count)),
            ]),
        ),
    ])
}

fn prompt_response(body: &[u8]) -> JsonValue {
    let payload: JsonValue = from_str(&String::from_utf8_lossy(body)).expect("prompt payload json");
    let prompt = payload
        .get("messages")
        .and_then(JsonValue::as_array)
        .and_then(|messages| messages.first())
        .and_then(|message| message.get("content"))
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let topic = topic_for_text(prompt);

    json_object(vec![
        ("id", JsonValue::String("chatcmpl_mock".to_string())),
        ("object", JsonValue::String("chat.completion".to_string())),
        ("model", JsonValue::String("gpt-4.1-mini".to_string())),
        (
            "choices",
            JsonValue::Array(vec![json_object(vec![
                ("index", JsonValue::Number(0.0)),
                ("finish_reason", JsonValue::String("stop".to_string())),
                (
                    "message",
                    json_object(vec![
                        ("role", JsonValue::String("assistant".to_string())),
                        ("content", JsonValue::String(topic.slug.to_string())),
                    ]),
                ),
            ])]),
        ),
        (
            "usage",
            json_object(vec![
                ("prompt_tokens", JsonValue::Number(10.0)),
                ("completion_tokens", JsonValue::Number(2.0)),
                ("total_tokens", JsonValue::Number(12.0)),
            ]),
        ),
    ])
}

fn spawn_mock_openai_server() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("mock ai listener should bind");
    let addr = listener
        .local_addr()
        .expect("mock ai listener should expose a local addr");

    let handle = thread::spawn(move || {
        for _ in 0..EXPECTED_AI_REQUESTS {
            let (mut stream, _) = listener.accept().expect("mock ai should accept");
            let (path, body) = read_http_request(&mut stream);
            let response = match path.as_str() {
                "/v1/embeddings" => embedding_response(&body),
                "/v1/chat/completions" => prompt_response(&body),
                other => panic!("unexpected mock ai path: {other}"),
            };
            write_http_json(&mut stream, &response);
        }
    });

    (format!("http://{addr}/v1"), handle)
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
            "Você está rotulando um cluster de comentários. Responda somente com um category_id curto em lowercase e com hífen.\nExemplos:\n{examples}"
        ));
    }

    (cluster_ids, prompts)
}

#[test]
fn e2e_comments_embedding_cluster_label_and_writeback() {
    let _env_lock = env_lock().lock().expect("env lock should be available");
    let (api_base, mock_ai) = spawn_mock_openai_server();
    let _env = EnvGuard::set(&[
        ("REDDB_AI_PROVIDER", "openai".to_string()),
        ("REDDB_OPENAI_API_BASE", api_base),
        ("REDDB_OPENAI_API_KEY", "test-key".to_string()),
        (
            "REDDB_OPENAI_EMBEDDING_MODEL",
            "text-embedding-3-small".to_string(),
        ),
        ("REDDB_OPENAI_PROMPT_MODEL", "gpt-4.1-mini".to_string()),
    ]);

    let rt = rt();
    seed_comments(&rt);
    let http_base = spawn_reddb_http(rt.clone());
    let query = QueryUseCases::new(&rt);

    let embedding_payload: JsonValue = from_str(
        r#"{
            "provider": "openai",
            "source_query": "SELECT red_entity_id, comment FROM comments ORDER BY red_entity_id ASC LIMIT 200",
            "source_mode": "row",
            "source_field": "comment",
            "source_collection": "comments",
            "max_inputs": 200,
            "save": {
                "collection": "comment_embeddings",
                "include_content": true,
                "metadata": {
                    "pipeline": "comment-clustering"
                }
            }
        }"#,
    )
    .expect("embedding payload json");
    let embeddings = http_post_json(&format!("{http_base}/ai/embeddings"), &embedding_payload);
    assert_eq!(
        embeddings.get("ok").and_then(JsonValue::as_bool),
        Some(true),
        "unexpected embeddings response: {}",
        embeddings
    );
    assert_eq!(
        embeddings
            .get("count")
            .and_then(JsonValue::as_i64)
            .unwrap_or_else(|| panic!("embedding count missing in response: {embeddings}")),
        200
    );

    let cluster_payload: JsonValue = from_str(
        r#"{
            "collection": "comment_embeddings",
            "algorithm": "kmeans",
            "k": 10,
            "max_iterations": 50
        }"#,
    )
    .expect("cluster payload json");
    let clusters = http_post_json(&format!("{http_base}/vectors/cluster"), &cluster_payload);
    assert_eq!(
        clusters.get("ok").and_then(JsonValue::as_bool),
        Some(true),
        "unexpected cluster response: {}",
        clusters
    );
    assert_eq!(
        clusters
            .get("total_vectors")
            .and_then(JsonValue::as_i64)
            .expect("total vectors"),
        200
    );

    let assignments = cluster_assignments(&clusters);
    assert_eq!(assignments.len(), 200);

    let (cluster_ids, prompts) = grouped_cluster_prompts(&assignments);
    assert_eq!(cluster_ids.len(), TOPIC_COUNT);

    let prompt_payload = json_object(vec![
        ("provider", JsonValue::String("openai".to_string())),
        ("model", JsonValue::String("gpt-4.1-mini".to_string())),
        (
            "prompts",
            JsonValue::Array(prompts.into_iter().map(JsonValue::String).collect()),
        ),
    ]);
    let labels = http_post_json(&format!("{http_base}/ai/prompt"), &prompt_payload);
    let outputs = labels
        .get("outputs")
        .and_then(JsonValue::as_array)
        .expect("prompt outputs");
    assert_eq!(outputs.len(), TOPIC_COUNT);

    let mut category_by_cluster = BTreeMap::new();
    for (cluster_id, output) in cluster_ids.iter().zip(outputs.iter()) {
        let category_id = output
            .get("text")
            .and_then(JsonValue::as_str)
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
                    category_id, assignment.linked_row_id
                ),
            })
            .unwrap_or_else(|err| panic!("category writeback should succeed: {err:?}"));
    }

    let grouped = query
        .execute(ExecuteQueryInput {
            query: "SELECT category_id, count(*) AS total FROM comments GROUP BY category_id ORDER BY category_id ASC".into(),
        })
        .expect("grouped category query should succeed");

    assert_eq!(grouped.result.records.len(), TOPIC_COUNT);
    for record in &grouped.result.records {
        let total = record
            .get("total")
            .and_then(|value| match value {
                Value::Integer(value) => Some(*value),
                Value::UnsignedInteger(value) => Some(*value as i64),
                _ => None,
            })
            .expect("group total");
        assert_eq!(total, COMMENTS_PER_TOPIC as i64);
    }

    let mut seen_categories = grouped
        .result
        .records
        .iter()
        .map(|record| match record.get("category_id") {
            Some(Value::Text(value)) => value.to_string(),
            other => panic!("expected category_id text, got {other:?}"),
        })
        .collect::<Vec<_>>();
    seen_categories.sort();

    let mut expected_categories = TOPICS
        .iter()
        .map(|topic| topic.slug.to_string())
        .collect::<Vec<_>>();
    expected_categories.sort();

    assert_eq!(seen_categories, expected_categories);

    mock_ai
        .join()
        .expect("mock ai server thread should exit cleanly");
}
