use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CACHE_VERSION: u32 = 3;
const DEFAULT_TEMPLATE: &str = include_str!("../assets/codex-token-metrics.html");

#[derive(Clone)]
struct Config {
    port: u16,
    template_path: PathBuf,
    cache_path: PathBuf,
    session_roots: Vec<PathBuf>,
    refresh_delay: Duration,
    poll_interval: Duration,
    env_model_pricing: BTreeMap<String, Pricing>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Pricing {
    input: f64,
    cached_input: f64,
    output: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Totals {
    input: u64,
    cached_input: u64,
    output: u64,
    reasoning_output: u64,
    total: u64,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct CostTotals {
    input_cost: f64,
    cached_input_cost: f64,
    output_cost: f64,
    total_cost: f64,
    uncached_input_tokens: u64,
    billed_output_tokens: u64,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct EstimatedCost {
    uncached_input_tokens: u64,
    billed_output_tokens: u64,
    input_cost: f64,
    cached_input_cost: f64,
    output_cost: f64,
    total_cost: f64,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PricingInfo {
    default: Pricing,
    model_overrides: BTreeMap<String, Pricing>,
    note: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ModelUsage {
    model: String,
    sessions: u64,
    turns: u64,
    total_tokens: u64,
    estimated_cost: EstimatedCost,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DailyRow {
    date: String,
    sessions: u64,
    input: u64,
    cached_input: u64,
    output: u64,
    reasoning_output: u64,
    total: u64,
    estimated_cost: EstimatedCost,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct TopSession {
    session_id: String,
    start_ts: Option<i64>,
    input: u64,
    cached_input: u64,
    output: u64,
    reasoning_output: u64,
    total: u64,
    model: Option<String>,
    estimated_cost: EstimatedCost,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct CacheStats {
    status: String,
    path: String,
    files_cached: u64,
    files_reparsed: u64,
    files_unreadable: u64,
    compute_duration_ms: u128,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Metrics {
    generated_at: String,
    files_scanned: usize,
    sessions_with_usage: usize,
    first_session_at: Option<String>,
    last_session_at: Option<String>,
    totals: Totals,
    cost_totals: CostTotals,
    pricing: PricingInfo,
    model_usage: Vec<ModelUsage>,
    daily: Vec<DailyRow>,
    top_sessions: Vec<TopSession>,
    cache: CacheStats,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Session {
    session_id: String,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
    input: u64,
    cached_input: u64,
    output: u64,
    reasoning_output: u64,
    total: u64,
    models: Vec<String>,
    primary_model: Option<String>,
    model_usage: BTreeMap<String, Totals>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ParsedFile {
    session: Option<Session>,
    #[serde(default)]
    model_turn_counts: BTreeMap<String, u64>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct FileCacheEntry {
    size: u64,
    mtime_ms: u64,
    parsed: ParsedFile,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PersistentCache {
    version: u32,
    saved_at: String,
    snapshot: Option<Metrics>,
    #[serde(default)]
    files: BTreeMap<String, FileCacheEntry>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct FilesSignature {
    files: usize,
    total_size: u64,
    max_mtime_ms: u64,
}

#[derive(Clone)]
struct SseMessage {
    event: &'static str,
    data: String,
}

struct Inner {
    file_metric_cache: BTreeMap<String, FileCacheEntry>,
    snapshot: Metrics,
    dirty: bool,
    refreshing: bool,
    last_signature: Option<FilesSignature>,
    sse_clients: Vec<mpsc::Sender<SseMessage>>,
}

struct AppState {
    config: Config,
    inner: Mutex<Inner>,
}

fn main() -> io::Result<()> {
    let config = Config::from_env();
    let (file_metric_cache, snapshot) = load_persistent_cache(&config);

    let state = Arc::new(AppState {
        inner: Mutex::new(Inner {
            file_metric_cache,
            snapshot,
            dirty: true,
            refreshing: false,
            last_signature: None,
            sse_clients: Vec::new(),
        }),
        config,
    });

    start_poll_watcher(Arc::clone(&state));

    let addr = format!("127.0.0.1:{}", state.config.port);
    let listener = TcpListener::bind(&addr)?;
    println!("Codex metrics live server running on http://{}", addr);
    println!("Template: {}", state.config.template_path.display());
    println!("Cache: {}", state.config.cache_path.display());
    println!("Mode: Rust binary cached snapshot + background refresh");
    println!("Press Ctrl+C to stop.");

    schedule_metrics_refresh(&state, "startup");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = Arc::clone(&state);
                thread::spawn(move || {
                    if let Err(err) = handle_connection(stream, state) {
                        eprintln!("connection error: {err}");
                    }
                });
            }
            Err(err) => eprintln!("accept error: {err}"),
        }
    }

    Ok(())
}

impl Config {
    fn from_env() -> Self {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let port = env::args()
            .nth(1)
            .or_else(|| env::var("CODEX_METRICS_PORT").ok())
            .or_else(|| env::var("PORT").ok())
            .and_then(|raw| raw.parse::<u16>().ok())
            .unwrap_or(8787);
        let template_path = env::var_os("CODEX_METRICS_TEMPLATE")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("codex-token-metrics.html"));
        let cache_path = env::var_os("CODEX_METRICS_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".cache"))
                    .join("codex-token-metrics-live")
                    .join("metrics-cache.json")
            });
        let refresh_delay = Duration::from_millis(env_u64("CODEX_METRICS_REFRESH_DELAY_MS", 100));
        let poll_interval = Duration::from_millis(env_u64("CODEX_METRICS_POLL_MS", 2_000));
        let env_model_pricing = parse_env_pricing_overrides();

        Self {
            session_roots: vec![
                home.join(".codex").join("sessions"),
                home.join(".codex").join("archived_sessions"),
            ],
            port,
            template_path,
            cache_path,
            refresh_delay,
            poll_interval,
            env_model_pricing,
        }
    }
}

fn env_u64(name: &str, fallback: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(fallback)
}

fn default_pricing() -> Pricing {
    Pricing {
        input: 1.75,
        cached_input: 0.175,
        output: 14.0,
    }
}

fn gpt_54_pricing() -> Pricing {
    Pricing {
        input: 2.5,
        cached_input: 0.25,
        output: 15.0,
    }
}

fn gpt_55_pricing() -> Pricing {
    Pricing {
        input: 5.0,
        cached_input: 0.5,
        output: 30.0,
    }
}

fn builtin_pricing() -> BTreeMap<String, Pricing> {
    let default = default_pricing();
    let mut prices = BTreeMap::new();
    prices.insert("gpt-5.3-codex".to_string(), default.clone());
    prices.insert("gpt-5.2-codex".to_string(), default.clone());
    prices.insert("gpt-5.2".to_string(), default.clone());
    prices.insert("gpt-5.5".to_string(), gpt_55_pricing());
    prices.insert("gpt-5.4".to_string(), gpt_54_pricing());
    prices.insert("gpt-5.1-codex-mini".to_string(), default.clone());
    prices.insert("gpt-5.3-codex-spark".to_string(), default);
    prices
}

fn parse_env_pricing_overrides() -> BTreeMap<String, Pricing> {
    env::var("CODEX_METRICS_RATES_JSON")
        .ok()
        .and_then(|raw| serde_json::from_str::<BTreeMap<String, Pricing>>(&raw).ok())
        .unwrap_or_default()
}

fn pricing_info(config: &Config) -> PricingInfo {
    let mut model_overrides = builtin_pricing();
    for (model, pricing) in &config.env_model_pricing {
        model_overrides.insert(model.clone(), pricing.clone());
    }

    PricingInfo {
        default: default_pricing(),
        model_overrides,
        note: "Estimated using uncached input formula: (input - cached) * input_rate + cached * cached_rate + (output + reasoning_output) * output_rate".to_string(),
    }
}

fn pricing_for_model(config: &Config, model: Option<&str>) -> Pricing {
    if let Some(model) = model {
        if let Some(pricing) = config.env_model_pricing.get(model) {
            return pricing.clone();
        }
        if let Some(pricing) = builtin_pricing().get(model) {
            return pricing.clone();
        }
    }
    default_pricing()
}

fn estimate_cost(
    input: u64,
    cached_input: u64,
    output: u64,
    reasoning_output: u64,
    pricing: &Pricing,
) -> EstimatedCost {
    let uncached_input_tokens = input.saturating_sub(cached_input);
    let billed_output_tokens = output.saturating_add(reasoning_output);
    let input_cost = (uncached_input_tokens as f64 / 1_000_000.0) * pricing.input;
    let cached_input_cost = (cached_input as f64 / 1_000_000.0) * pricing.cached_input;
    let output_cost = (billed_output_tokens as f64 / 1_000_000.0) * pricing.output;

    EstimatedCost {
        uncached_input_tokens,
        billed_output_tokens,
        input_cost,
        cached_input_cost,
        output_cost,
        total_cost: input_cost + cached_input_cost + output_cost,
    }
}

fn empty_metrics(config: &Config, status: &str) -> Metrics {
    Metrics {
        generated_at: now_iso(),
        files_scanned: 0,
        sessions_with_usage: 0,
        first_session_at: None,
        last_session_at: None,
        totals: Totals::default(),
        cost_totals: CostTotals::default(),
        pricing: pricing_info(config),
        model_usage: Vec::new(),
        daily: Vec::new(),
        top_sessions: Vec::new(),
        cache: CacheStats {
            status: status.to_string(),
            path: config.cache_path.display().to_string(),
            ..CacheStats::default()
        },
    }
}

fn load_persistent_cache(config: &Config) -> (BTreeMap<String, FileCacheEntry>, Metrics) {
    let fallback = empty_metrics(config, "warming");
    let bytes = match fs::read(&config.cache_path) {
        Ok(bytes) => bytes,
        Err(_) => return (BTreeMap::new(), fallback),
    };
    let parsed = match serde_json::from_slice::<PersistentCache>(&bytes) {
        Ok(parsed) if parsed.version == CACHE_VERSION => parsed,
        Ok(parsed) => {
            eprintln!(
                "ignoring metrics cache version {}, expected {}",
                parsed.version, CACHE_VERSION
            );
            return (BTreeMap::new(), fallback);
        }
        Err(err) => {
            eprintln!("ignoring unreadable metrics cache: {err}");
            return (BTreeMap::new(), fallback);
        }
    };

    let mut snapshot = parsed.snapshot.unwrap_or(fallback);
    snapshot.cache.status = "cached".to_string();
    snapshot.cache.path = config.cache_path.display().to_string();
    (parsed.files, snapshot)
}

fn save_persistent_cache(
    config: &Config,
    file_metric_cache: &BTreeMap<String, FileCacheEntry>,
    snapshot: &Metrics,
) -> io::Result<()> {
    if let Some(parent) = config.cache_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload = PersistentCache {
        version: CACHE_VERSION,
        saved_at: now_iso(),
        snapshot: Some(snapshot.clone()),
        files: file_metric_cache.clone(),
    };
    let tmp = config
        .cache_path
        .with_file_name(format!("metrics-cache.json.{}.tmp", std::process::id()));
    fs::write(&tmp, serde_json::to_vec(&payload)?)?;
    fs::rename(tmp, &config.cache_path)?;
    Ok(())
}

fn schedule_metrics_refresh(state: &Arc<AppState>, reason: &'static str) {
    let file_metric_cache = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        if inner.refreshing {
            inner.dirty = true;
            return;
        }
        inner.refreshing = true;
        inner.dirty = false;
        inner.file_metric_cache.clone()
    };

    let state = Arc::clone(state);
    thread::spawn(move || {
        thread::sleep(state.config.refresh_delay);
        let (snapshot, next_cache, signature) = compute_metrics(&state.config, file_metric_cache);
        if let Err(err) = save_persistent_cache(&state.config, &next_cache, &snapshot) {
            eprintln!("failed to save metrics cache: {err}");
        }

        let needs_reschedule = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            inner.snapshot = snapshot.clone();
            inner.file_metric_cache = next_cache;
            inner.last_signature = Some(signature);
            let needs_reschedule = inner.dirty;
            inner.refreshing = false;
            if !needs_reschedule {
                inner.dirty = false;
            }
            needs_reschedule
        };

        broadcast(
            &state,
            SseMessage {
                event: "update",
                data: format!(
                    "{{\"at\":{},\"reason\":\"{}\",\"cache\":{}}}",
                    unix_ms(),
                    reason,
                    serde_json::to_string(&snapshot.cache).unwrap_or_else(|_| "{}".to_string())
                ),
            },
        );

        if needs_reschedule {
            schedule_metrics_refresh(&state, "queued");
        }
    });
}

fn get_metrics_snapshot(state: &Arc<AppState>) -> Metrics {
    let (snapshot, should_refresh) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        (inner.snapshot.clone(), inner.dirty && !inner.refreshing)
    };
    if should_refresh {
        schedule_metrics_refresh(state, "request");
    }
    snapshot
}

fn start_poll_watcher(state: Arc<AppState>) {
    thread::spawn(move || loop {
        thread::sleep(state.config.poll_interval);
        let signature = scan_signature(&state.config.session_roots);
        let changed = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            match inner.last_signature {
                Some(last) if last == signature => false,
                Some(_) => {
                    inner.last_signature = Some(signature);
                    inner.dirty = true;
                    true
                }
                None => {
                    inner.last_signature = Some(signature);
                    false
                }
            }
        };
        if changed {
            schedule_metrics_refresh(&state, "filesystem");
        }
    });
}

fn compute_metrics(
    config: &Config,
    previous_cache: BTreeMap<String, FileCacheEntry>,
) -> (Metrics, BTreeMap<String, FileCacheEntry>, FilesSignature) {
    let started_at = Instant::now();
    let files = session_files(&config.session_roots);
    let mut signature = FilesSignature::default();
    signature.files = files.len();

    let mut next_cache = BTreeMap::new();
    let mut parsed_files = Vec::with_capacity(files.len());
    let mut files_cached = 0_u64;
    let mut files_reparsed = 0_u64;
    let mut files_unreadable = 0_u64;

    for file in files {
        let metadata = match fs::metadata(&file) {
            Ok(metadata) => metadata,
            Err(_) => {
                files_unreadable += 1;
                continue;
            }
        };
        let size = metadata.len();
        let mtime_ms = metadata_mtime_ms(&metadata);
        signature.total_size = signature.total_size.saturating_add(size);
        signature.max_mtime_ms = signature.max_mtime_ms.max(mtime_ms);

        let key = file.display().to_string();
        let entry = match previous_cache.get(&key) {
            Some(cached) if cached.size == size && cached.mtime_ms == mtime_ms => {
                files_cached += 1;
                cached.clone()
            }
            _ => {
                files_reparsed += 1;
                FileCacheEntry {
                    size,
                    mtime_ms,
                    parsed: parse_session_file(&file),
                }
            }
        };
        parsed_files.push(entry.parsed.clone());
        next_cache.insert(key, entry);
    }

    let mut snapshot = aggregate_metrics(config, signature.files, parsed_files);
    snapshot.cache = CacheStats {
        status: "fresh".to_string(),
        path: config.cache_path.display().to_string(),
        files_cached,
        files_reparsed,
        files_unreadable,
        compute_duration_ms: started_at.elapsed().as_millis(),
    };

    (snapshot, next_cache, signature)
}

fn aggregate_metrics(
    config: &Config,
    files_scanned: usize,
    parsed_files: Vec<ParsedFile>,
) -> Metrics {
    let mut sessions = Vec::new();
    let mut model_turn_counts: BTreeMap<String, u64> = BTreeMap::new();

    for parsed in parsed_files {
        for (model, turns) in parsed.model_turn_counts {
            *model_turn_counts.entry(model).or_insert(0) += turns;
        }
        if let Some(session) = parsed.session {
            sessions.push(session);
        }
    }

    sessions.sort_by_key(|session| session.start_ts.unwrap_or(0));

    let mut totals = Totals::default();
    for session in &sessions {
        totals.input = totals.input.saturating_add(session.input);
        totals.cached_input = totals.cached_input.saturating_add(session.cached_input);
        totals.output = totals.output.saturating_add(session.output);
        totals.reasoning_output = totals
            .reasoning_output
            .saturating_add(session.reasoning_output);
        totals.total = totals.total.saturating_add(session.total);
    }

    let mut cost_totals = CostTotals::default();
    let mut daily_map: BTreeMap<String, DailyRow> = BTreeMap::new();
    let mut model_stats: BTreeMap<String, ModelUsage> = BTreeMap::new();
    let mut sessions_with_cost = Vec::with_capacity(sessions.len());

    for mut session in sessions {
        let mut session_estimated_cost = EstimatedCost::default();
        let model_usage = session_model_usage(&session);
        for (model, usage) in &model_usage {
            let pricing = pricing_for_model(config, Some(model));
            let estimated_cost = estimate_cost(
                usage.input,
                usage.cached_input,
                usage.output,
                usage.reasoning_output,
                &pricing,
            );

            cost_totals.input_cost += estimated_cost.input_cost;
            cost_totals.cached_input_cost += estimated_cost.cached_input_cost;
            cost_totals.output_cost += estimated_cost.output_cost;
            cost_totals.total_cost += estimated_cost.total_cost;
            cost_totals.uncached_input_tokens = cost_totals
                .uncached_input_tokens
                .saturating_add(estimated_cost.uncached_input_tokens);
            cost_totals.billed_output_tokens = cost_totals
                .billed_output_tokens
                .saturating_add(estimated_cost.billed_output_tokens);
            add_estimated_cost(&mut session_estimated_cost, &estimated_cost);

            let stat = model_stats
                .entry(model.clone())
                .or_insert_with(|| ModelUsage {
                    model: model.clone(),
                    ..ModelUsage::default()
                });
            stat.sessions += 1;
            stat.total_tokens = stat.total_tokens.saturating_add(usage.total);
            add_estimated_cost(&mut stat.estimated_cost, &estimated_cost);
        }

        if let Some(day) = session
            .start_ts
            .or(session.end_ts)
            .and_then(day_from_millis)
        {
            let row = daily_map.entry(day.clone()).or_insert_with(|| DailyRow {
                date: day,
                ..DailyRow::default()
            });
            row.sessions += 1;
            row.input = row.input.saturating_add(session.input);
            row.cached_input = row.cached_input.saturating_add(session.cached_input);
            row.output = row.output.saturating_add(session.output);
            row.reasoning_output = row
                .reasoning_output
                .saturating_add(session.reasoning_output);
            row.total = row.total.saturating_add(session.total);
            add_estimated_cost(&mut row.estimated_cost, &session_estimated_cost);
        }

        let model_label = session_model_label(&session, &model_usage);
        sessions_with_cost.push((
            TopSession {
                session_id: std::mem::take(&mut session.session_id),
                start_ts: session.start_ts,
                input: session.input,
                cached_input: session.cached_input,
                output: session.output,
                reasoning_output: session.reasoning_output,
                total: session.total,
                model: model_label,
                estimated_cost: session_estimated_cost,
            },
            session.start_ts,
            session.end_ts,
        ));
    }

    for (model, turns) in model_turn_counts {
        let stat = model_stats
            .entry(model.clone())
            .or_insert_with(|| ModelUsage {
                model,
                ..ModelUsage::default()
            });
        stat.turns = turns;
    }

    let mut model_usage: Vec<ModelUsage> = model_stats.into_values().collect();
    model_usage.sort_by(|a, b| {
        b.sessions
            .cmp(&a.sessions)
            .then_with(|| b.turns.cmp(&a.turns))
            .then_with(|| a.model.cmp(&b.model))
    });

    let daily = daily_map.into_values().collect();

    let mut top_sessions: Vec<TopSession> = sessions_with_cost
        .iter()
        .map(|(session, _, _)| session.clone())
        .collect();
    top_sessions.sort_by(|a, b| b.total.cmp(&a.total));
    top_sessions.truncate(15);

    let first_session_at = sessions_with_cost
        .first()
        .and_then(|(_, start, _)| start.and_then(millis_to_iso));
    let last_session_at = sessions_with_cost
        .last()
        .and_then(|(_, _, end)| end.and_then(millis_to_iso));

    Metrics {
        generated_at: now_iso(),
        files_scanned,
        sessions_with_usage: sessions_with_cost.len(),
        first_session_at,
        last_session_at,
        totals,
        cost_totals,
        pricing: pricing_info(config),
        model_usage,
        daily,
        top_sessions,
        cache: CacheStats::default(),
    }
}

fn add_estimated_cost(target: &mut EstimatedCost, add: &EstimatedCost) {
    target.uncached_input_tokens = target
        .uncached_input_tokens
        .saturating_add(add.uncached_input_tokens);
    target.billed_output_tokens = target
        .billed_output_tokens
        .saturating_add(add.billed_output_tokens);
    target.input_cost += add.input_cost;
    target.cached_input_cost += add.cached_input_cost;
    target.output_cost += add.output_cost;
    target.total_cost += add.total_cost;
}

fn session_model_usage(session: &Session) -> BTreeMap<String, Totals> {
    if !session.model_usage.is_empty() {
        return session.model_usage.clone();
    }

    let model = session
        .primary_model
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    BTreeMap::from([(
        model,
        Totals {
            input: session.input,
            cached_input: session.cached_input,
            output: session.output,
            reasoning_output: session.reasoning_output,
            total: session.total,
        },
    )])
}

fn session_model_label(
    session: &Session,
    model_usage: &BTreeMap<String, Totals>,
) -> Option<String> {
    if model_usage.len() == 1 {
        return model_usage.keys().next().cloned();
    }
    if model_usage.len() > 1 {
        return Some("mixed".to_string());
    }
    session.primary_model.clone()
}

fn parse_session_file(file: &Path) -> ParsedFile {
    let input = match File::open(file) {
        Ok(file) => file,
        Err(_) => return ParsedFile::default(),
    };
    let reader = BufReader::new(input);

    let mut session_id = None;
    let mut start_ts = None;
    let mut end_ts = None;
    let mut input = 0_u64;
    let mut cached_input = 0_u64;
    let mut output = 0_u64;
    let mut reasoning_output = 0_u64;
    let mut total = 0_u64;
    let mut has_token_usage = false;
    let mut models = Vec::<String>::new();
    let mut model_usage: BTreeMap<String, Totals> = BTreeMap::new();
    let mut model_turn_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut primary_model = None;
    let mut current_model = None;
    let mut previous_total_usage: Option<Totals> = None;

    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let row = match serde_json::from_str::<Value>(&line) {
            Ok(row) => row,
            Err(_) => continue,
        };

        if let Some(ts) = row
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_ts_millis)
        {
            start_ts = Some(start_ts.map_or(ts, |current: i64| current.min(ts)));
            end_ts = Some(end_ts.map_or(ts, |current: i64| current.max(ts)));
        }

        if row.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(id) = row
                .pointer("/payload/id")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                session_id = Some(id);
            }
            if let Some(ts) = row
                .pointer("/payload/timestamp")
                .and_then(Value::as_str)
                .and_then(parse_ts_millis)
            {
                start_ts = Some(start_ts.map_or(ts, |current: i64| current.min(ts)));
            }
        }

        if row.get("type").and_then(Value::as_str) == Some("turn_context") {
            if let Some(model) = row.pointer("/payload/model").and_then(Value::as_str) {
                let model = model.to_string();
                if !models.iter().any(|known| known == &model) {
                    models.push(model.clone());
                }
                if primary_model.is_none() {
                    primary_model = Some(model.clone());
                }
                current_model = Some(model.clone());
                *model_turn_counts.entry(model).or_insert(0) += 1;
            }
        }

        if row.get("type").and_then(Value::as_str) != Some("event_msg")
            || row.pointer("/payload/type").and_then(Value::as_str) != Some("token_count")
        {
            continue;
        }
        let total_usage = match parse_usage(row.pointer("/payload/info/total_token_usage")) {
            Some(usage) => usage,
            None => continue,
        };

        has_token_usage = true;
        input = input.max(total_usage.input);
        cached_input = cached_input.max(total_usage.cached_input);
        output = output.max(total_usage.output);
        reasoning_output = reasoning_output.max(total_usage.reasoning_output);
        total = total.max(total_usage.total);

        if previous_total_usage.as_ref() == Some(&total_usage) {
            continue;
        }

        let delta_usage = previous_total_usage
            .as_ref()
            .map(|previous| usage_delta(&total_usage, previous))
            .unwrap_or_else(|| total_usage.clone());
        previous_total_usage = Some(total_usage);

        let last_usage = parse_usage(row.pointer("/payload/info/last_token_usage"));
        let turn_usage = match last_usage {
            Some(last_usage) if usage_has_billable_tokens(&last_usage) => last_usage,
            _ => delta_usage,
        };
        if !usage_has_billable_tokens(&turn_usage) {
            continue;
        }

        let model = current_model
            .clone()
            .or_else(|| primary_model.clone())
            .unwrap_or_else(|| "unknown".to_string());
        if !models.iter().any(|known| known == &model) {
            models.push(model.clone());
        }
        add_totals(model_usage.entry(model).or_default(), &turn_usage);
    }

    let session = if has_token_usage {
        Some(Session {
            session_id: session_id.unwrap_or_else(|| {
                file.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown")
                    .trim_start_matches("rollout-")
                    .trim_end_matches(".jsonl")
                    .to_string()
            }),
            start_ts,
            end_ts,
            input,
            cached_input,
            output,
            reasoning_output,
            total,
            models,
            primary_model,
            model_usage,
        })
    } else {
        None
    };

    ParsedFile {
        session,
        model_turn_counts,
    }
}

fn session_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in roots {
        walk_session_files(root, &mut files);
    }
    files
}

fn walk_session_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            walk_session_files(&path, files);
        } else if file_type.is_file() {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                files.push(path);
            }
        }
    }
}

fn scan_signature(roots: &[PathBuf]) -> FilesSignature {
    let mut signature = FilesSignature::default();
    for file in session_files(roots) {
        let Ok(metadata) = fs::metadata(file) else {
            continue;
        };
        signature.files += 1;
        signature.total_size = signature.total_size.saturating_add(metadata.len());
        signature.max_mtime_ms = signature.max_mtime_ms.max(metadata_mtime_ms(&metadata));
    }
    signature
}

fn metadata_mtime_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn handle_connection(mut stream: TcpStream, state: Arc<AppState>) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    if first_line.trim().is_empty() {
        return Ok(());
    }

    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }

    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or("/");

    if method != "GET" && method != "HEAD" {
        write_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"Method not allowed",
        )?;
        return Ok(());
    }

    match path {
        "/events" => handle_events(stream, state),
        "/api/data" => {
            let payload = serde_json::to_vec(&get_metrics_snapshot(&state))?;
            write_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                if method == "HEAD" { b"" } else { &payload },
            )
        }
        "/" | "/index.html" => {
            let html = render_live_html(&state);
            write_response(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                if method == "HEAD" {
                    b""
                } else {
                    html.as_bytes()
                },
            )
        }
        _ => write_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"Not found",
        ),
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn handle_events(mut stream: TcpStream, state: Arc<AppState>) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.sse_clients.push(tx);
    }

    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache, no-transform\r\nConnection: keep-alive\r\n\r\n: connected\r\n\r\n"
    )?;
    stream.flush()?;

    loop {
        match rx.recv_timeout(Duration::from_secs(25)) {
            Ok(message) => {
                write!(
                    stream,
                    "event: {}\ndata: {}\n\n",
                    message.event, message.data
                )?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                write!(stream, "event: ping\ndata: {{\"at\":{}}}\n\n", unix_ms())?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if stream.flush().is_err() {
            break;
        }
    }

    Ok(())
}

fn broadcast(state: &Arc<AppState>, message: SseMessage) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    inner
        .sse_clients
        .retain(|sender| sender.send(message.clone()).is_ok());
}

fn render_live_html(state: &Arc<AppState>) -> String {
    let data = get_metrics_snapshot(state);
    let template = fs::read_to_string(&state.config.template_path)
        .unwrap_or_else(|_| DEFAULT_TEMPLATE.to_string());
    inject_realtime_client(&inject_data(&template, &data))
}

fn inject_data(template: &str, data: &Metrics) -> String {
    let replacement = format!(
        "const data = {};",
        serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string())
    );
    for pattern in [
        r"(?s)const\s+data\s*=\s*\{.*?\};",
        r"(?s)const\s+data\s*=\s*\[.*?\];",
    ] {
        let re = Regex::new(pattern).expect("valid regex");
        if re.is_match(template) {
            return re.replace(template, replacement.as_str()).to_string();
        }
    }
    template.replace(
        "</body>",
        &format!("<script>{replacement}</script>\n</body>"),
    )
}

fn inject_realtime_client(html: &str) -> String {
    if html.contains("id=\"codex-live-sse\"") {
        return html.to_string();
    }
    let snippet = r#"<script id="codex-live-sse">
(() => {
  const es = new EventSource('/events');
  let reloading = false;
  let refreshInFlight = null;
  const reloadNow = () => {
    if (reloading) return;
    reloading = true;
    window.location.reload();
  };
  const refreshNow = async () => {
    if (reloading) return;
    const refresh = window.refreshLiveData;
    if (typeof refresh !== 'function') {
      reloadNow();
      return;
    }
    if (refreshInFlight) return refreshInFlight;
    refreshInFlight = Promise.resolve()
      .then(() => refresh())
      .catch(() => {
        reloadNow();
      })
      .finally(() => {
        refreshInFlight = null;
      });
    return refreshInFlight;
  };
  es.addEventListener('update', () => {
    void refreshNow();
  });
  es.addEventListener('template', reloadNow);
  es.addEventListener('error', () => {
    if (es.readyState === EventSource.CLOSED) reloadNow();
  });
})();
</script>"#;
    html.replace("</body>", &format!("{snippet}\n</body>"))
}

fn value_u64(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(0),
        Some(Value::String(raw)) => raw.parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

fn parse_usage(value: Option<&Value>) -> Option<Totals> {
    let Value::Object(usage) = value? else {
        return None;
    };
    Some(Totals {
        input: value_u64(usage.get("input_tokens")),
        cached_input: value_u64(usage.get("cached_input_tokens")),
        output: value_u64(usage.get("output_tokens")),
        reasoning_output: value_u64(usage.get("reasoning_output_tokens")),
        total: value_u64(usage.get("total_tokens")),
    })
}

fn usage_delta(current: &Totals, previous: &Totals) -> Totals {
    Totals {
        input: current.input.saturating_sub(previous.input),
        cached_input: current.cached_input.saturating_sub(previous.cached_input),
        output: current.output.saturating_sub(previous.output),
        reasoning_output: current
            .reasoning_output
            .saturating_sub(previous.reasoning_output),
        total: current.total.saturating_sub(previous.total),
    }
}

fn usage_has_billable_tokens(usage: &Totals) -> bool {
    usage.input > 0 || usage.cached_input > 0 || usage.output > 0 || usage.reasoning_output > 0
}

fn add_totals(target: &mut Totals, add: &Totals) {
    target.input = target.input.saturating_add(add.input);
    target.cached_input = target.cached_input.saturating_add(add.cached_input);
    target.output = target.output.saturating_add(add.output);
    target.reasoning_output = target.reasoning_output.saturating_add(add.reasoning_output);
    target.total = target.total.saturating_add(add.total);
}

fn parse_ts_millis(raw: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).timestamp_millis())
}

fn millis_to_iso(ms: i64) -> Option<String> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn day_from_millis(ms: i64) -> Option<String> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_token_usage_to_active_turn_model() {
        let path =
            env::temp_dir().join(format!("codex-token-metrics-live-test-{}.jsonl", unix_ms()));
        let jsonl = [
            r#"{"timestamp":"2026-05-10T00:00:00.000Z","type":"session_meta","payload":{"id":"test-session","timestamp":"2026-05-10T00:00:00.000Z"}}"#,
            r#"{"timestamp":"2026-05-10T00:00:01.000Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
            r#"{"timestamp":"2026-05-10T00:00:02.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":20,"reasoning_output_tokens":5,"total_tokens":120},"last_token_usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":20,"reasoning_output_tokens":5,"total_tokens":120}}}}"#,
            r#"{"timestamp":"2026-05-10T00:00:03.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":20,"reasoning_output_tokens":5,"total_tokens":120},"last_token_usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":20,"reasoning_output_tokens":5,"total_tokens":120}}}}"#,
            r#"{"timestamp":"2026-05-10T00:00:04.000Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
            r#"{"timestamp":"2026-05-10T00:00:05.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":250,"cached_input_tokens":60,"output_tokens":50,"reasoning_output_tokens":10,"total_tokens":300},"last_token_usage":{"input_tokens":150,"cached_input_tokens":50,"output_tokens":30,"reasoning_output_tokens":5,"total_tokens":180}}}}"#,
        ]
        .join("\n");

        fs::write(&path, jsonl).expect("write test session");
        let parsed = parse_session_file(&path);
        fs::remove_file(&path).ok();

        let session = parsed.session.expect("session with usage");
        assert_eq!(session.input, 250);
        assert_eq!(session.cached_input, 60);
        assert_eq!(session.output, 50);
        assert_eq!(session.reasoning_output, 10);
        assert_eq!(session.total, 300);
        assert_eq!(parsed.model_turn_counts.get("gpt-5.5"), Some(&1));
        assert_eq!(parsed.model_turn_counts.get("gpt-5.4"), Some(&1));

        let gpt_55 = session.model_usage.get("gpt-5.5").expect("gpt-5.5 usage");
        assert_eq!(gpt_55.input, 100);
        assert_eq!(gpt_55.cached_input, 10);
        assert_eq!(gpt_55.output, 20);
        assert_eq!(gpt_55.reasoning_output, 5);
        assert_eq!(gpt_55.total, 120);

        let gpt_54 = session.model_usage.get("gpt-5.4").expect("gpt-5.4 usage");
        assert_eq!(gpt_54.input, 150);
        assert_eq!(gpt_54.cached_input, 50);
        assert_eq!(gpt_54.output, 30);
        assert_eq!(gpt_54.reasoning_output, 5);
        assert_eq!(gpt_54.total, 180);
    }
}
