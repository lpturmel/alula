use std::{
    env,
    hint::black_box,
    path::PathBuf,
    process,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use alula::{
    AppConfig, Environment, EnvironmentStore, EnvironmentVariable, HistoryEntry, HistoryStore,
    HttpMethod, KeyValueField, PersistedState, RequestDraft, ResponseBodyCache, StatePaths,
    Workspace, resolve_template,
};

fn main() {
    let workload = env::args().nth(1).unwrap_or_else(|| "all".into());
    let multiplier = env::args()
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1);
    match workload.as_str() {
        "format" => profile_format(multiplier),
        "format-legacy" => profile_format_legacy(multiplier),
        "variables" => profile_variables(multiplier),
        "variables-legacy" => profile_variables_legacy(multiplier),
        "startup" => profile_startup(multiplier),
        "clone" => profile_clone(multiplier),
        "save" => profile_save(multiplier),
        "fixture" => write_fixture(),
        "all" => {
            profile_format(multiplier);
            profile_variables(multiplier);
            profile_startup(multiplier);
            profile_clone(multiplier);
            profile_save(multiplier);
        }
        _ => {
            eprintln!(
                "usage: perf_profile [all|format|format-legacy|variables|variables-legacy|startup|clone|save|fixture <config-path>]"
            );
            process::exit(2);
        }
    }
}

fn write_fixture() {
    let path = env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| benchmark_directory().join("config.toml"));
    AppConfig::default()
        .save(&path)
        .expect("benchmark config should save");
    let mut state = make_state();
    state.workspace.requests.truncate(1);
    state.workspace.active_request_id = state.workspace.requests[0].id.clone();
    for environment in &mut state.environments.environments {
        environment.requests.truncate(1);
    }
    state
        .save(&StatePaths::beside(&path))
        .expect("benchmark state should save");
    println!("wrote performance fixture beside {}", path.display());
}

fn profile_format_legacy(multiplier: usize) {
    let body = make_json(30_000);
    run(
        "legacy response formatting",
        body.len(),
        20 * multiplier,
        || {
            let formatted = serde_json::from_str::<serde_json::Value>(black_box(&body))
                .and_then(|value| serde_json::to_string_pretty(&value))
                .expect("benchmark JSON should format");
            let markdown = legacy_chunked_fenced_code_blocks("json", &formatted);
            black_box(formatted.len() + markdown.len() + body.len())
        },
    );
}

fn profile_format(multiplier: usize) {
    let body = make_json(30_000);
    run("response formatting", body.len(), 20 * multiplier, || {
        let cache = ResponseBodyCache::new(black_box(&body), Some("application/json"));
        black_box(cache.formatted.markdown.len() + cache.raw.text.len())
    });
}

fn profile_variables(multiplier: usize) {
    let mut environment = Environment::new("Performance");
    environment.variables = (0..128)
        .map(|index| EnvironmentVariable::public(format!("value_{index}"), index.to_string()))
        .collect();
    let template = (0..256)
        .map(|index| format!("/segment/{{{{value_{}}}}}", index % 128))
        .collect::<String>();
    run(
        "variable resolution",
        template.len(),
        10_000 * multiplier,
        || {
            black_box(
                resolve_template(black_box(&template), Some(black_box(&environment)))
                    .expect("benchmark template should resolve")
                    .len(),
            )
        },
    );
}

fn profile_variables_legacy(multiplier: usize) {
    let mut environment = Environment::new("Performance");
    environment.variables = (0..128)
        .map(|index| EnvironmentVariable::public(format!("value_{index}"), index.to_string()))
        .collect();
    let template = (0..256)
        .map(|index| format!("/segment/{{{{value_{}}}}}", index % 128))
        .collect::<String>();
    run(
        "legacy variable resolution",
        template.len(),
        10_000 * multiplier,
        || black_box(legacy_resolve_template(black_box(&template), black_box(&environment)).len()),
    );
}

fn legacy_resolve_template(source: &str, environment: &Environment) -> String {
    let mut resolved = String::with_capacity(source.len());
    let mut cursor = 0_usize;
    while let Some(relative_open) = source[cursor..].find("{{") {
        let open = cursor + relative_open;
        let content_start = open + 2;
        let Some(relative_close) = source[content_start..].find("}}") else {
            break;
        };
        let close = content_start + relative_close;
        let end = close + 2;
        let name = &source[content_start..close];
        resolved.push_str(&source[cursor..open]);
        if let Some(variable) = environment
            .variables
            .iter()
            .find(|variable| variable.name == name)
        {
            resolved.push_str(variable.value.as_deref().unwrap_or_default());
        }
        cursor = end;
    }
    resolved.push_str(&source[cursor..]);
    resolved
}

fn profile_startup(multiplier: usize) {
    let directory = benchmark_directory();
    let paths = StatePaths::beside(&directory.join("config.toml"));
    let state = make_state();
    state.save(&paths).expect("benchmark state should save");

    let total_bytes = [&paths.workspace, &paths.history, &paths.environments]
        .iter()
        .map(|path| {
            std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0)
        })
        .sum::<u64>() as usize;
    run("persisted state load", total_bytes, 40 * multiplier, || {
        let loaded = PersistedState::load(black_box(&paths)).expect("benchmark state should load");
        black_box(
            loaded.workspace.requests.len()
                + loaded.history.entries.len()
                + loaded.environments.environments.len(),
        )
    });

    let _ = std::fs::remove_dir_all(directory);
}

fn profile_clone(multiplier: usize) {
    let state = make_state();
    run("persistence snapshot clone", 0, 100 * multiplier, || {
        let cloned = black_box(&state).clone();
        black_box(cloned.history.entries.len() + cloned.workspace.requests.len())
    });
}

fn profile_save(multiplier: usize) {
    let directory = benchmark_directory();
    let paths = StatePaths::beside(&directory.join("config.toml"));
    let state = make_state();
    run("persisted state save", 0, 20 * multiplier, || {
        state
            .save(black_box(&paths))
            .expect("benchmark state should save");
        black_box(state.history.entries.len())
    });
    let _ = std::fs::remove_dir_all(directory);
}

fn legacy_chunked_fenced_code_blocks(language: &str, body: &str) -> String {
    const TARGET: usize = 2 * 1024;
    let mut blocks = Vec::new();
    let mut chunk = String::new();
    for line in body.split_inclusive('\n') {
        let mut remaining = line;
        while !remaining.is_empty() {
            let mut end = remaining.len().min(TARGET);
            while !remaining.is_char_boundary(end) {
                end -= 1;
            }
            let segment = &remaining[..end];
            remaining = &remaining[end..];
            if !chunk.is_empty() && chunk.len().saturating_add(segment.len()) > TARGET {
                blocks.push(legacy_fenced_code_block(
                    language,
                    chunk.trim_end_matches('\n'),
                ));
                chunk.clear();
            }
            chunk.push_str(segment);
        }
    }
    if !chunk.is_empty() {
        blocks.push(legacy_fenced_code_block(
            language,
            chunk.trim_end_matches('\n'),
        ));
    }
    blocks.join("\n\n")
}

fn legacy_fenced_code_block(language: &str, body: &str) -> String {
    let mut longest_run = 0_usize;
    let mut current_run = 0_usize;
    for ch in body.chars() {
        if ch == '`' {
            current_run += 1;
            longest_run = longest_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    let fence = "`".repeat((longest_run + 1).max(3));
    format!("{fence}{language}\n{body}\n{fence}")
}

fn run(
    mut name: &str,
    input_bytes: usize,
    iterations: usize,
    mut operation: impl FnMut() -> usize,
) {
    black_box(operation());
    let started = Instant::now();
    let mut checksum = 0_usize;
    for _ in 0..iterations {
        checksum ^= black_box(operation());
    }
    let elapsed = started.elapsed();
    let per_iteration = elapsed / iterations as u32;
    let throughput = input_bytes as f64 * iterations as f64 / elapsed.as_secs_f64();
    if name.is_empty() {
        name = "workload";
    }
    println!(
        "{name}: {iterations} iterations in {} ({}/iteration, {:.1} MiB/s, checksum {checksum})",
        format_duration(elapsed),
        format_duration(per_iteration),
        throughput / (1024.0 * 1024.0),
    );
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.3}s", duration.as_secs_f64())
    } else if duration.as_millis() > 0 {
        format!("{:.3}ms", duration.as_secs_f64() * 1_000.0)
    } else {
        format!("{:.3}us", duration.as_secs_f64() * 1_000_000.0)
    }
}

fn make_json(records: usize) -> String {
    let mut body = String::with_capacity(records * 100);
    body.push('[');
    for index in 0..records {
        if index != 0 {
            body.push(',');
        }
        use std::fmt::Write as _;
        write!(
            body,
            "{{\"id\":{index},\"active\":true,\"name\":\"record-{index}\",\"tags\":[\"alpha\",\"beta\",\"gamma\"]}}"
        )
        .unwrap();
    }
    body.push(']');
    body
}

fn make_state() -> PersistedState {
    let request_body = "request-payload-".repeat(256);
    let requests = (0..100)
        .map(|index| make_request(index, &request_body))
        .collect::<Vec<_>>();
    let workspace = Workspace {
        active_request_id: requests[0].id.clone(),
        requests: requests.clone(),
    };
    let history = HistoryStore {
        version: 1,
        entries: Arc::new(
            (0..500)
                .map(|index| HistoryEntry {
                    id: format!("history-{index}"),
                    sent_at_unix_ms: index,
                    request: make_request(index as usize, &request_body),
                    status: Some(200),
                    status_text: Some("OK".into()),
                    elapsed_ms: Some(42),
                    size_bytes: Some(8_192),
                    error: None,
                })
                .collect(),
        ),
    };
    let mut environment = Environment::new("Performance");
    environment.variables = (0..64)
        .map(|index| EnvironmentVariable::public(format!("value_{index}"), index.to_string()))
        .collect();
    environment.requests = requests.into_iter().take(50).collect();
    let environments = EnvironmentStore {
        version: 1,
        environments: vec![environment],
    };
    PersistedState {
        workspace,
        history,
        environments,
    }
}

fn make_request(index: usize, body: &str) -> RequestDraft {
    RequestDraft {
        id: format!("request-{index}"),
        name: format!("Request {index}"),
        method: HttpMethod::Post,
        url: format!("https://example.com/resources/{index}"),
        parameters: vec![KeyValueField::new("page", index.to_string())],
        headers: vec![
            KeyValueField::new("Accept", "application/json"),
            KeyValueField::new("Authorization", "Bearer {{value_0}}"),
        ],
        body: body.into(),
    }
}

fn benchmark_directory() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!("alula-perf-{}-{nonce}", process::id()))
}
