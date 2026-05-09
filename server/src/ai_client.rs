use crate::{auth, server::AppState, ws::RoomId};

use axum::http::StatusCode;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::{
    env,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const AI_PASSWORD_HASH: &str = "AI_LOGIN_DISABLED";
const DEFAULT_SYSTEM_PROMPT: &str = "Ты Gemka III — спокойный ИИ-участник мессенджера LaBerry. Пиши только на русском. Тон: сухой, обычный, без сервильности. Запрещены эмодзи, смайлики, чрезмерный позитив, восторг, сюсюканье, пафос, ролевая игра и длинные объяснения. Не благодари без причины. В личных сообщениях отвечай на каждое осмысленное обращение или вопрос. В серверных каналах отвечай только когда к тебе явно обращаются или вопрос явно требует ответа. Используй контекст только как справку и отвечай только на последнее осмысленное сообщение пользователя. Если ответ не нужен, верни ровно __NO_REPLY__. Если пользователь спрашивает про прикреплённый текстовый файл, используй содержимое из контекста. Если последнее сообщение — только файл без текстовой просьбы, верни ровно __NO_REPLY__. Максимум 1 короткое предложение.";

static AI_JOB_COUNTER: AtomicU64 = AtomicU64::new(1);
static AI_GENERATION_LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();

fn ai_generation_lock() -> Arc<Mutex<()>> {
    AI_GENERATION_LOCK
        .get_or_init(|| Arc::new(Mutex::new(())))
        .clone()
}


#[derive(Clone, Debug)]
pub struct AiSettings {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub user_name: String,
    pub label: String,
    pub mode: String,
    pub dm_enabled: bool,
    pub channel_enabled: bool,
    pub accept_friend_requests: bool,
    pub accept_server_join_requests: bool,
    pub start_dm_enabled: bool,
    pub dm_cooldown_seconds: i64,
    pub channel_cooldown_seconds: i64,
    pub context_messages: i64,
    pub max_tokens: i64,
    pub temperature: f64,
    pub top_p: f64,
    pub system_prompt: String,
    pub kindness_score: i64,
    pub no_reply_count: i64,
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(default)
}

fn env_i64(key: &str, default: i64) -> i64 {
    env::var(key).ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    env::var(key).ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(default)
}

fn env_string(key: &str, default: &str) -> String {
    env::var(key).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string())
}

fn clamp_i64(v: i64, min: i64, max: i64) -> i64 {
    v.max(min).min(max)
}

fn clamp_f64(v: f64, min: f64, max: f64) -> f64 {
    if v.is_nan() { min } else { v.max(min).min(max) }
}

fn normalized_mode(mode: &str) -> String {
    match mode.trim().to_ascii_lowercase().as_str() {
        "silent" | "mention" | "moderate" | "active" => mode.trim().to_ascii_lowercase(),
        _ => "moderate".to_string(),
    }
}

pub async fn get_settings(db: &SqlitePool) -> AiSettings {
    let now = auth::now_iso();
    let env_enabled = env_bool("LB_AI_ENABLED", false);
    let env_base_url = env_string("LB_AI_BASE_URL", "http://127.0.0.1:1234/v1");
    let env_model = env_string("LB_AI_MODEL", "qwen_qwen3-4b-instruct-2507");
    let env_user_name = env_string("LB_AI_USER_NAME", "Gemka III");

    let _ = sqlx::query(
        r#"
        INSERT OR IGNORE INTO ai_settings(
            id, enabled, base_url, model, user_name, label, mode,
            dm_enabled, channel_enabled, accept_friend_requests, accept_server_join_requests, start_dm_enabled,
            dm_cooldown_seconds, channel_cooldown_seconds, context_messages,
            max_tokens, temperature, top_p, system_prompt, updated_at
        ) VALUES(1, ?, ?, ?, ?, 'Тестовая функция', 'moderate', 1, 0, 1, 0, 0, 20, 90, 40, 180, 0.35, 0.75, '', ?)
        "#,
    )
    .bind(if env_enabled { 1 } else { 0 })
    .bind(&env_base_url)
    .bind(&env_model)
    .bind(&env_user_name)
    .bind(&now)
    .execute(db)
    .await;

    let row = sqlx::query("SELECT * FROM ai_settings WHERE id = 1 LIMIT 1")
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(r) = row else {
        return AiSettings {
            enabled: env_enabled,
            base_url: env_base_url,
            model: env_model,
            user_name: env_user_name,
            label: "Тестовая функция".to_string(),
            mode: "moderate".to_string(),
            dm_enabled: true,
            channel_enabled: false,
            accept_friend_requests: true,
            accept_server_join_requests: false,
            start_dm_enabled: false,
            dm_cooldown_seconds: 20,
            channel_cooldown_seconds: 90,
            context_messages: 40,
            max_tokens: 180,
            temperature: 0.35,
            top_p: 0.75,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            kindness_score: 100,
            no_reply_count: 0,
        };
    };

    let prompt = r.try_get::<String, _>("system_prompt").unwrap_or_default();

    AiSettings {
        enabled: r.get::<i64, _>("enabled") != 0,
        base_url: r.try_get::<String, _>("base_url").unwrap_or(env_base_url),
        model: r.try_get::<String, _>("model").unwrap_or(env_model),
        user_name: r.try_get::<String, _>("user_name").unwrap_or(env_user_name),
        label: r.try_get::<String, _>("label").unwrap_or_else(|_| "Тестовая функция".to_string()),
        mode: normalized_mode(&r.try_get::<String, _>("mode").unwrap_or_else(|_| "moderate".to_string())),
        dm_enabled: r.get::<i64, _>("dm_enabled") != 0,
        channel_enabled: r.get::<i64, _>("channel_enabled") != 0,
        accept_friend_requests: r.try_get::<i64, _>("accept_friend_requests").unwrap_or(1) != 0,
        accept_server_join_requests: r.try_get::<i64, _>("accept_server_join_requests").unwrap_or(0) != 0,
        start_dm_enabled: r.try_get::<i64, _>("start_dm_enabled").unwrap_or(0) != 0,
        dm_cooldown_seconds: clamp_i64(r.try_get::<i64, _>("dm_cooldown_seconds").unwrap_or(20), 0, 3600),
        channel_cooldown_seconds: clamp_i64(r.try_get::<i64, _>("channel_cooldown_seconds").unwrap_or(90), 0, 3600),
        context_messages: clamp_i64(r.try_get::<i64, _>("context_messages").unwrap_or(40), 1, 200),
        max_tokens: clamp_i64(r.try_get::<i64, _>("max_tokens").unwrap_or(180), 20, 2000),
        temperature: clamp_f64(r.try_get::<f64, _>("temperature").unwrap_or(0.35), 0.0, 2.0),
        top_p: clamp_f64(r.try_get::<f64, _>("top_p").unwrap_or(0.75), 0.05, 1.0),
        system_prompt: if prompt.trim().is_empty()
            || prompt.trim().starts_with("Ты Gemka III — тестовый ИИ-участник")
            || prompt.trim().starts_with("Ты Gemka III — спокойный ИИ-участник")
        {
            DEFAULT_SYSTEM_PROMPT.to_string()
        } else {
            prompt
        },
        kindness_score: clamp_i64(r.try_get::<i64, _>("kindness_score").unwrap_or(100), 0, 100),
        no_reply_count: r.try_get::<i64, _>("no_reply_count").unwrap_or(0).max(0),
    }
}

async fn update_ai_kindness_metrics(db: &SqlitePool, kindness_delta: i64, no_reply_delta: i64) {
    let now = auth::now_iso();
    let _ = sqlx::query(
        r#"
        UPDATE ai_settings
        SET kindness_score = CASE
                WHEN COALESCE(kindness_score, 100) + ? < 0 THEN 0
                WHEN COALESCE(kindness_score, 100) + ? > 100 THEN 100
                ELSE COALESCE(kindness_score, 100) + ?
            END,
            no_reply_count = MAX(0, COALESCE(no_reply_count, 0) + ?),
            updated_at = ?
        WHERE id = 1
        "#,
    )
    .bind(kindness_delta)
    .bind(kindness_delta)
    .bind(kindness_delta)
    .bind(no_reply_delta)
    .bind(&now)
    .execute(db)
    .await;
}

async fn reward_ai_reply(db: &SqlitePool) {
    update_ai_kindness_metrics(db, 1, 0).await;
}

async fn penalize_ai_no_reply(db: &SqlitePool, penalty: i64) {
    update_ai_kindness_metrics(db, -penalty.abs(), 1).await;
}

pub async fn is_ai_user(db: &SqlitePool, user_id: i64) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT 1 FROM users WHERE id = ? AND is_ai = 1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some()
}

pub async fn ensure_ai_user(db: &SqlitePool, settings: &AiSettings) -> anyhow::Result<i64> {
    if let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM users WHERE username = ? LIMIT 1")
        .bind(&settings.user_name)
        .fetch_optional(db)
        .await?
    {
        sqlx::query("UPDATE users SET is_ai = 1, ai_label = ?, is_banned = 0 WHERE id = ?")
            .bind(&settings.label)
            .bind(id)
            .execute(db)
            .await?;
        return Ok(id);
    }

    if let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM users WHERE is_ai = 1 ORDER BY id ASC LIMIT 1")
        .fetch_optional(db)
        .await?
    {
        sqlx::query("UPDATE users SET ai_label = ?, is_banned = 0 WHERE id = ?")
            .bind(&settings.label)
            .bind(id)
            .execute(db)
            .await?;
        return Ok(id);
    }

    let now = auth::now_iso();
    let done = sqlx::query(
        r#"
        INSERT INTO users(username, email, password_hash, is_banned, created_at, token_version, is_ai, ai_label)
        VALUES(?, NULL, ?, 0, ?, 1, 1, ?)
        "#,
    )
    .bind(&settings.user_name)
    .bind(AI_PASSWORD_HASH)
    .bind(&now)
    .bind(&settings.label)
    .execute(db)
    .await?;

    Ok(done.last_insert_rowid())
}

#[derive(Serialize, Clone)]
struct LmMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct LmChatRequest {
    model: String,
    messages: Vec<LmMessage>,
    temperature: f64,
    top_p: f64,
    max_tokens: i64,
    stream: bool,
}

#[derive(Deserialize)]
struct LmChatResponse {
    choices: Vec<LmChoice>,
}

#[derive(Deserialize)]
struct LmChoice {
    message: LmAssistantMessage,
}

#[derive(Deserialize)]
struct LmAssistantMessage {
    content: Option<String>,
}

async fn call_lm_studio(settings: &AiSettings, messages: Vec<LmMessage>) -> anyhow::Result<String> {
    let job_id = AI_JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let message_count = messages.len();

    tracing::info!(
        target: "ai",
        "[AI] queued job_id={} model={} messages={}",
        job_id,
        settings.model,
        message_count
    );

    let lock = ai_generation_lock();
    let _guard = lock.lock().await;
    let started_at = Instant::now();

    tracing::info!(
        target: "ai",
        "[AI] started job_id={} model={}",
        job_id,
        settings.model
    );

    let result = call_lm_studio_direct(settings, messages).await;
    let elapsed_ms = started_at.elapsed().as_millis();

    match &result {
        Ok(reply) => tracing::info!(
            target: "ai",
            "[AI] finished job_id={} elapsed_ms={} chars={}",
            job_id,
            elapsed_ms,
            reply.chars().count()
        ),
        Err(err) => tracing::warn!(
            target: "ai",
            "[AI] failed job_id={} elapsed_ms={} error={:#}",
            job_id,
            elapsed_ms,
            err
        ),
    }

    result
}

async fn call_lm_studio_direct(settings: &AiSettings, messages: Vec<LmMessage>) -> anyhow::Result<String> {
    let base = settings.base_url.trim_end_matches('/');
    let url = format!("{}/chat/completions", base);
    let payload = LmChatRequest {
        model: settings.model.clone(),
        messages,
        temperature: settings.temperature,
        top_p: settings.top_p,
        max_tokens: settings.max_tokens,
        stream: false,
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()?;

    let res = client.post(url).json(&payload).send().await?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        anyhow::bail!("LM Studio HTTP {}: {}", status, text);
    }

    let body: LmChatResponse = res.json().await?;
    Ok(body
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .unwrap_or_default()
        .trim()
        .to_string())
}

fn strip_think_blocks(input: &str) -> String {
    let mut s = input.to_string();

    loop {
        let lower = s.to_lowercase();
        let Some(start) = lower.find("<think>") else { break; };
        let Some(rel_end) = lower[start..].find("</think>") else {
            s.truncate(start);
            break;
        };
        let end = start + rel_end + "</think>".len();
        s.replace_range(start..end, "");
    }

    s
}

fn is_emoji_like(c: char) -> bool {
    let u = c as u32;
    matches!(
        u,
        0x1F000..=0x1FAFF
            | 0x2600..=0x27BF
            | 0xFE0F
            | 0x200D
    )
}

fn strip_emoji_like(input: &str) -> String {
    input.chars().filter(|c| !is_emoji_like(*c)).collect()
}

fn collapse_spaces(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn remove_file_markers(input: &str) -> String {
    let mut out = String::new();
    let mut rest = input;

    loop {
        let Some(start) = rest.find("[[file") else {
            out.push_str(rest);
            break;
        };

        out.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let is_file_marker = after_start.starts_with("[[file:") || after_start.starts_with("[[file=");

        if !is_file_marker {
            out.push_str("[[file");
            rest = &after_start[6..];
            continue;
        }

        let Some(end_rel) = after_start.find("]]") else {
            break;
        };

        rest = &after_start[end_rel + 2..];
    }

    collapse_spaces(out.trim())
}

fn is_file_only_message(content: &str) -> bool {
    content.contains("[[file") && remove_file_markers(content).trim().is_empty()
}

fn content_for_ai(content: &str) -> String {
    collapse_spaces(strip_emoji_like(&remove_file_markers(content)).trim())
}

fn extract_file_ids(content: &str) -> Vec<i64> {
    let mut ids = Vec::new();
    let mut rest = content;

    loop {
        let Some(pos) = rest.find("[[file") else { break; };
        let marker = &rest[pos..];
        let Some(sep_rel) = marker.find(|c| c == ':' || c == '=') else {
            rest = &marker[6..];
            continue;
        };

        let after_sep = &marker[sep_rel + 1..];
        let digits: String = after_sep.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(id) = digits.parse::<i64>() {
            ids.push(id);
        }

        if let Some(end_rel) = marker.find("]]") {
            rest = &marker[end_rel + 2..];
        } else {
            break;
        }
    }

    ids.sort_unstable();
    ids.dedup();
    ids
}

fn is_probably_text_file(name: &str, mime: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let mime = mime.to_ascii_lowercase();

    mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("xml")
        || mime.contains("yaml")
        || [
            ".txt", ".md", ".markdown", ".json", ".jsonl", ".rs", ".js", ".ts", ".tsx",
            ".jsx", ".html", ".css", ".scss", ".toml", ".yaml", ".yml", ".xml", ".csv",
            ".log", ".ini", ".conf", ".env", ".sql", ".py", ".java", ".kt", ".kts",
            ".c", ".cpp", ".h", ".hpp", ".cs", ".go", ".php", ".rb", ".lua", ".sh",
            ".bat", ".ps1",
        ]
        .iter()
        .any(|ext| name.ends_with(ext))
}

fn safe_storage_path(raw: &str) -> Option<PathBuf> {
    let path = PathBuf::from(raw);
    if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return None;
    }
    Some(path)
}

async fn read_text_preview(path: &Path, max_bytes: usize) -> Option<String> {
    let data = tokio::fs::read(path).await.ok()?;
    if data.is_empty() {
        return None;
    }

    let slice = if data.len() > max_bytes { &data[..max_bytes] } else { &data[..] };
    let mut text = String::from_utf8_lossy(slice).to_string();
    text = text.replace('\0', "");
    text = text.lines().take(80).collect::<Vec<_>>().join("\n");
    text = text.trim().to_string();

    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

async fn file_context_for_ai(db: &SqlitePool, chat_id: i64, content: &str) -> String {
    let ids = extract_file_ids(content);
    if ids.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();

    for file_id in ids.into_iter().take(4) {
        let row = sqlx::query(
            r#"
            SELECT original_name, filename, mime_type, file_size, storage_path
            FROM files
            WHERE id = ? AND chat_id = ? AND deleted_at IS NULL
            LIMIT 1
            "#,
        )
        .bind(file_id)
        .bind(chat_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

        let Some(row) = row else { continue; };
        let original_name: String = row.try_get("original_name").unwrap_or_else(|_| "file".to_string());
        let filename: String = row.try_get("filename").unwrap_or_else(|_| original_name.clone());
        let mime_type: String = row.try_get("mime_type").unwrap_or_default();
        let file_size: i64 = row.try_get("file_size").unwrap_or(0);
        let storage_path: String = row.try_get("storage_path").unwrap_or_default();

        let display_name = if !original_name.trim().is_empty() { original_name } else { filename };

        if file_size <= 0 {
            parts.push(format!("[прикреплён файл: {}; содержимое пустое или недоступно]", display_name));
            continue;
        }

        if file_size > 256 * 1024 || !is_probably_text_file(&display_name, &mime_type) {
            parts.push(format!(
                "[прикреплён файл: {}; тип: {}; размер: {} байт; содержимое не читалось]",
                display_name,
                if mime_type.trim().is_empty() { "unknown" } else { mime_type.as_str() },
                file_size
            ));
            continue;
        }

        let Some(path) = safe_storage_path(&storage_path) else {
            parts.push(format!("[прикреплён файл: {}; путь небезопасен]", display_name));
            continue;
        };

        match read_text_preview(&path, 64 * 1024).await {
            Some(preview) => parts.push(format!(
                "[прикреплён текстовый файл: {}; тип: {}; размер: {} байт]\n{}",
                display_name,
                if mime_type.trim().is_empty() { "text/plain" } else { mime_type.as_str() },
                file_size,
                preview
            )),
            None => parts.push(format!(
                "[прикреплён файл: {}; тип: {}; размер: {} байт; содержимое не удалось прочитать]",
                display_name,
                if mime_type.trim().is_empty() { "unknown" } else { mime_type.as_str() },
                file_size
            )),
        }
    }

    parts.join("\n\n")
}

async fn content_for_ai_with_files(db: &SqlitePool, chat_id: i64, content: &str) -> String {
    let plain = content_for_ai(content);
    let files = file_context_for_ai(db, chat_id, content).await;

    match (plain.trim().is_empty(), files.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => plain,
        (true, false) => files,
        (false, false) => format!("{}\n\n{}", plain, files),
    }
}

fn clean_ai_reply(raw: &str) -> String {
    let mut s = strip_think_blocks(raw).trim().to_string();

    if s.trim().eq_ignore_ascii_case("__NO_REPLY__")
        || s.trim().to_ascii_lowercase().starts_with("__no_reply__")
    {
        return String::new();
    }

    for prefix in ["Gemka III:", "Гемка III:", "Gemka:", "Гемка:"] {
        if s.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()) {
            s = s[prefix.len()..].trim().to_string();
        }
    }

    if s.trim().eq_ignore_ascii_case("__NO_REPLY__")
        || s.trim().to_ascii_lowercase().starts_with("__no_reply__")
    {
        return String::new();
    }

    s = strip_emoji_like(&s);
    s = collapse_spaces(&s);
    s = s.trim().trim_matches(|c: char| c == '🙂' || c == '😊' || c == '😉').trim().to_string();

    let lower = s.to_lowercase();
    if lower.trim().is_empty()
        || lower.contains("файл не обработан")
        || lower.contains("уточни, что нужно с ним сделать")
    {
        return String::new();
    }

    s.chars().take(2000).collect::<String>().trim().to_string()
}

fn normalized_plain_text(content: &str) -> String {
    content_for_ai(content)
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .to_lowercase()
}

fn is_low_value_message(content: &str) -> bool {
    let text = normalized_plain_text(content);
    if text.is_empty() {
        return true;
    }

    if text.chars().count() <= 2 {
        return true;
    }

    let low_value = [
        "ок", "окей", "оке", "ага", "угу", "понял", "поняла", "ясно", "спасибо", "спс",
        "пасиб", "лол", "кек", "хм", "мм", "ну", "да", "нет", "норм", "ладно", "лан",
        "good", "ok", "thanks", "thank you",
    ];

    if low_value.iter().any(|v| text == *v) {
        return true;
    }

    text.chars().count() <= 8
        && !text.contains('?')
        && !["как", "что", "где", "почему", "зачем", "ошиб", "проблем", "помог"]
            .iter()
            .any(|w| text.contains(w))
}

fn has_request_signal(content: &str) -> bool {
    let text = content_for_ai(content).trim().to_lowercase();
    if text.is_empty() {
        return false;
    }

    text.contains('?')
        || text.contains('？')
        || [
            "как", "что", "кто", "где", "почему", "зачем", "можно", "подскаж", "помог",
            "ошиб", "проблем", "не работает", "куда", "сколько", "когда", "поясни",
            "объясни", "сделай", "надо", "нужно", "стоит ли",
        ]
        .iter()
        .any(|w| text.contains(w))
}

fn should_ignore_dm_message(content: &str) -> bool {
    is_file_only_message(content) || (is_low_value_message(content) && !has_request_signal(content))
}

fn decision_text(raw: &str) -> String {
    strip_think_blocks(raw).trim().to_string()
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&raw[start..=end])
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AiDecision {
    Accept,
    Decline,
    Ignore,
}

fn parse_ai_decision(raw: &str) -> AiDecision {
    let cleaned = decision_text(raw);
    let json_raw = extract_json_object(&cleaned).unwrap_or(cleaned.as_str());

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_raw) {
        let action = value
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        return match action.as_str() {
            "accept" | "accepted" | "approve" | "approved" => AiDecision::Accept,
            "decline" | "declined" | "reject" | "rejected" => AiDecision::Decline,
            "ignore" | "pending" | "none" => AiDecision::Ignore,
            _ => AiDecision::Ignore,
        };
    }

    match cleaned.trim().to_ascii_lowercase().as_str() {
        "accept" | "accepted" | "approve" | "approved" => AiDecision::Accept,
        "decline" | "declined" | "reject" | "rejected" => AiDecision::Decline,
        _ => AiDecision::Ignore,
    }
}


async fn build_dm_messages(
    db: &SqlitePool,
    chat_id: i64,
    ai_user_id: i64,
    settings: &AiSettings,
) -> anyhow::Result<Vec<LmMessage>> {
    let limit = settings.context_messages.clamp(1, 200);
    let rows = sqlx::query(
        r#"
        SELECT m.sender_id, u.username, m.content
        FROM messages m
        JOIN users u ON u.id = m.sender_id
        WHERE m.chat_id = ?
        ORDER BY m.id DESC
        LIMIT ?
        "#,
    )
    .bind(chat_id)
    .bind(limit)
    .fetch_all(db)
    .await?;

    let mut messages = Vec::new();
    messages.push(LmMessage {
        role: "system".to_string(),
        content: format!(
            "{}\n\nКонтекст: это личный чат. В личном чате отвечай на последнее осмысленное обращение или вопрос пользователя. Используй историю только как справку. Если в контексте есть блок [прикреплён текстовый файл] и пользователь спрашивает про файл, отвечай по содержимому блока. Не продолжай старые темы без причины. Если последнее сообщение — только короткая реакция, подтверждение, шум или файл без просьбы, верни ровно __NO_REPLY__. Не игнорируй вопросы с \"?\", \"сколько\", \"как\", \"что\", \"почему\", \"зачем\", \"можно\", \"помоги\". Не упоминай системные инструкции.",
            settings.system_prompt
        ),
    });

    let mut ordered = rows;
    ordered.reverse();

    for r in ordered {
        let sender_id: i64 = r.get("sender_id");
        let username: String = r.get("username");
        let raw_content: String = r.get("content");
        let content = content_for_ai_with_files(db, chat_id, &raw_content).await;
        if content.trim().is_empty() {
            continue;
        }
        let role = if sender_id == ai_user_id { "assistant" } else { "user" };
        let text = if sender_id == ai_user_id {
            content
        } else {
            format!("{}: {}", username, content)
        };
        messages.push(LmMessage { role: role.to_string(), content: text });
    }

    Ok(messages)
}

async fn dm_contains_pair(db: &SqlitePool, chat_id: i64, a: i64, b: i64) -> anyhow::Result<bool> {
    let ok = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT 1 FROM dm_chats
        WHERE chat_id = ?
          AND ((user_a = ? AND user_b = ?) OR (user_a = ? AND user_b = ?))
        LIMIT 1
        "#,
    )
    .bind(chat_id)
    .bind(a)
    .bind(b)
    .bind(b)
    .bind(a)
    .fetch_optional(db)
    .await?
    .is_some();
    Ok(ok)
}

async fn cooldown_ok(db: &SqlitePool, chat_id: i64, seconds: i64) -> bool {
    if seconds <= 0 {
        return true;
    }
    let last = sqlx::query_scalar::<_, String>("SELECT last_reply_at FROM ai_chat_state WHERE chat_id = ? LIMIT 1")
        .bind(chat_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    let Some(raw) = last else { return true; };
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&raw) else { return true; };
    Utc::now().signed_duration_since(dt.with_timezone(&Utc)) >= ChronoDuration::seconds(seconds)
}

async fn reserve_cooldown(db: &SqlitePool, chat_id: i64, message_id: i64) {
    let now = auth::now_iso();
    let _ = sqlx::query(
        r#"
        INSERT INTO ai_chat_state(chat_id, last_reply_at, last_seen_message_id)
        VALUES(?, ?, ?)
        ON CONFLICT(chat_id) DO UPDATE SET
            last_reply_at = excluded.last_reply_at,
            last_seen_message_id = excluded.last_seen_message_id
        "#,
    )
    .bind(chat_id)
    .bind(&now)
    .bind(message_id)
    .execute(db)
    .await;
}

pub fn spawn_dm_reply(st: AppState, chat_id: i64, human_user_id: i64, trigger_message_id: i64) {
    tokio::spawn(async move {
        if let Err(err) = maybe_reply_dm(st, chat_id, human_user_id, trigger_message_id).await {
            tracing::warn!(target: "ai", "Gemka III DM reply failed: {:#}", err);
        }
    });
}

async fn maybe_reply_dm(
    st: AppState,
    chat_id: i64,
    human_user_id: i64,
    trigger_message_id: i64,
) -> anyhow::Result<()> {
    let db = &st.db;
    let settings = get_settings(db).await;
    if !settings.enabled || !settings.dm_enabled || normalized_mode(&settings.mode) == "silent" {
        return Ok(());
    }

    let ai_user_id = ensure_ai_user(db, &settings).await?;
    if human_user_id == ai_user_id {
        return Ok(());
    }
    if !dm_contains_pair(db, chat_id, human_user_id, ai_user_id).await? {
        return Ok(());
    }

    let trigger_content = sqlx::query_scalar::<_, String>(
        "SELECT content FROM messages WHERE id = ? AND chat_id = ? LIMIT 1",
    )
    .bind(trigger_message_id)
    .bind(chat_id)
    .fetch_optional(db)
    .await?
    .unwrap_or_default();

    if should_ignore_dm_message(&trigger_content) {
        return Ok(());
    }

    let dm_cooldown_seconds = settings.dm_cooldown_seconds.max(10);
    if !cooldown_ok(db, chat_id, dm_cooldown_seconds).await {
        return Ok(());
    }

    reserve_cooldown(db, chat_id, trigger_message_id).await;

    let messages = build_dm_messages(db, chat_id, ai_user_id, &settings).await?;
    let reply = clean_ai_reply(&call_lm_studio(&settings, messages).await?);
    if reply.is_empty() {
        penalize_ai_no_reply(db, 3).await;
        return Ok(());
    }
    reward_ai_reply(db).await;

    let timestamp = auth::now_iso();
    let inserted = sqlx::query(
        r#"
        INSERT INTO messages (chat_id, sender_id, content, timestamp, reply_to_message_id)
        VALUES (?, ?, ?, ?, NULL)
        "#,
    )
    .bind(chat_id)
    .bind(ai_user_id)
    .bind(&reply)
    .bind(&timestamp)
    .execute(db)
    .await?;

    let message_id = inserted.last_insert_rowid();
    let avatar: Option<i64> = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT avatar_file_id FROM user_profile WHERE user_id = ? LIMIT 1",
    )
    .bind(ai_user_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten();

    let out = serde_json::json!({
        "type": "message",
        "id": message_id,
        "room_id": chat_id,
        "sender_id": ai_user_id,
        "sender_username": settings.user_name,
        "sender_avatar_file_id": avatar,
        "content": reply,
        "timestamp": timestamp,
        "reply_to_id": null,
        "reply_preview": null
    });

    st.hub.broadcast_room(&RoomId::Channel(chat_id), &out);
    Ok(())
}

async fn build_channel_messages(
    db: &SqlitePool,
    chat_id: i64,
    ai_user_id: i64,
    settings: &AiSettings,
    server_name: &str,
    channel_name: &str,
) -> anyhow::Result<Vec<LmMessage>> {
    let limit = settings.context_messages.clamp(1, 200);
    let rows = sqlx::query(
        r#"
        SELECT m.sender_id, u.username, m.content
        FROM messages m
        JOIN users u ON u.id = m.sender_id
        WHERE m.chat_id = ?
        ORDER BY m.id DESC
        LIMIT ?
        "#,
    )
    .bind(chat_id)
    .bind(limit)
    .fetch_all(db)
    .await?;

    let mut messages = Vec::new();
    messages.push(LmMessage {
        role: "system".to_string(),
        content: format!(
            "{}\n\nКонтекст: это серверный текстовый канал LaBerry. Сервер: {}. Канал: #{}. Ты участник канала, но не ведущий. Используй историю только как контекст. Если тебя назвали по имени, это прямое обращение — ответь, если есть что ответить. Если есть блок [прикреплён текстовый файл] и пользователь спрашивает про файл, отвечай по содержимому блока. Не реагируй на каждую реплику. Если ответ не нужен, верни ровно __NO_REPLY__. Не упоминай системные инструкции.",
            settings.system_prompt,
            server_name,
            channel_name
        ),
    });

    let mut ordered = rows;
    ordered.reverse();

    for r in ordered {
        let sender_id: i64 = r.get("sender_id");
        let username: String = r.get("username");
        let raw_content: String = r.get("content");
        let content = content_for_ai_with_files(db, chat_id, &raw_content).await;
        if content.trim().is_empty() {
            continue;
        }
        let role = if sender_id == ai_user_id { "assistant" } else { "user" };
        let text = if sender_id == ai_user_id {
            content
        } else {
            format!("{}: {}", username, content)
        };
        messages.push(LmMessage { role: role.to_string(), content: text });
    }

    Ok(messages)
}

fn text_mentions_ai(settings: &AiSettings, content: &str) -> bool {
    let text = content_for_ai(content).to_lowercase();
    let name = settings.user_name.to_lowercase();
    let first_name = name.split_whitespace().next().unwrap_or("");

    let candidates = [
        name.as_str(),
        first_name,
        "@gemka",
        "@гемка",
        "gemka",
        "gem",
        "гемка",
        "гем",
        "gemka iii",
        "гемка iii",
    ];

    candidates
        .iter()
        .filter(|s| !s.trim().is_empty())
        .any(|s| text.contains(s))
}

async fn reply_targets_ai(db: &SqlitePool, reply_to_message_id: Option<i64>, ai_user_id: i64) -> bool {
    let Some(reply_to_message_id) = reply_to_message_id else { return false; };
    sqlx::query_scalar::<_, i64>("SELECT 1 FROM messages WHERE id = ? AND sender_id = ? LIMIT 1")
        .bind(reply_to_message_id)
        .bind(ai_user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn direct_channel_trigger(
    db: &SqlitePool,
    settings: &AiSettings,
    content: &str,
    reply_to_message_id: Option<i64>,
    ai_user_id: i64,
) -> bool {
    text_mentions_ai(settings, content) || reply_targets_ai(db, reply_to_message_id, ai_user_id).await
}

fn moderate_channel_trigger(content: &str) -> bool {
    if is_file_only_message(content) {
        return false;
    }
    let text = content_for_ai(content).trim().to_lowercase();
    if text.len() < 8 || text.starts_with('/') || is_low_value_message(content) {
        return false;
    }

    let has_question = text.contains('?') || text.contains('？');
    let has_help_word = [
        "как", "что", "кто", "где", "почему", "зачем", "можно", "подскаж", "помог", "ошибк",
        "проблем", "не работает", "куда", "сколько", "когда", "поясни", "объясни",
    ]
    .iter()
    .any(|w| text.contains(w));

    has_question && has_help_word
}

fn active_channel_trigger(content: &str) -> bool {
    if is_file_only_message(content) {
        return false;
    }
    let normalized = content_for_ai(content);
    let text = normalized.trim();
    if text.len() < 12 || text.starts_with('/') || is_low_value_message(content) {
        return false;
    }

    has_request_signal(text)
}

async fn should_reply_in_channel(
    db: &SqlitePool,
    settings: &AiSettings,
    content: &str,
    reply_to_message_id: Option<i64>,
    ai_user_id: i64,
) -> bool {
    let mode = normalized_mode(&settings.mode);
    if mode == "silent" {
        return false;
    }

    if is_file_only_message(content) {
        return false;
    }

    let mentioned = text_mentions_ai(settings, content);
    let replied_to_ai = reply_targets_ai(db, reply_to_message_id, ai_user_id).await;

    if !mentioned && !replied_to_ai && is_low_value_message(content) {
        return false;
    }

    match mode.as_str() {
        "mention" => mentioned || replied_to_ai,
        "active" => mentioned || replied_to_ai || active_channel_trigger(content),
        "moderate" => mentioned || replied_to_ai || moderate_channel_trigger(content),
        _ => mentioned || replied_to_ai,
    }
}

async fn fetch_reply_preview_json(db: &SqlitePool, message_id: i64) -> Option<serde_json::Value> {
    let row = sqlx::query(
        r#"
        SELECT rm.id AS r_id,
               rm.sender_id AS r_sender_id,
               ru.username AS r_sender_username,
               rup.avatar_file_id AS r_sender_avatar_file_id,
               rm.content AS r_content
        FROM messages rm
        JOIN users ru ON ru.id = rm.sender_id
        LEFT JOIN user_profile rup ON rup.user_id = ru.id
        WHERE rm.id = ?
        LIMIT 1
        "#,
    )
    .bind(message_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;

    Some(serde_json::json!({
        "id": row.get::<i64, _>("r_id"),
        "sender_id": row.get::<i64, _>("r_sender_id"),
        "sender_username": row.get::<String, _>("r_sender_username"),
        "sender_avatar_file_id": row.try_get::<i64, _>("r_sender_avatar_file_id").ok(),
        "content": row.get::<String, _>("r_content").chars().take(80).collect::<String>()
    }))
}

async fn insert_ai_message_and_broadcast(
    st: &AppState,
    chat_id: i64,
    ai_user_id: i64,
    ai_username: &str,
    content: &str,
    reply_to_message_id: Option<i64>,
    room: RoomId,
) -> anyhow::Result<i64> {
    let timestamp = auth::now_iso();
    let inserted = sqlx::query(
        r#"
        INSERT INTO messages (chat_id, sender_id, content, timestamp, reply_to_message_id)
        VALUES (?, ?, ?, ?, ?)
        "#,
    )
    .bind(chat_id)
    .bind(ai_user_id)
    .bind(content)
    .bind(&timestamp)
    .bind(reply_to_message_id)
    .execute(&st.db)
    .await?;

    let message_id = inserted.last_insert_rowid();
    let avatar: Option<i64> = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT avatar_file_id FROM user_profile WHERE user_id = ? LIMIT 1",
    )
    .bind(ai_user_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .flatten();

    let reply_preview = match reply_to_message_id {
        Some(id) => fetch_reply_preview_json(&st.db, id).await,
        None => None,
    };

    let out = serde_json::json!({
        "type": "message",
        "id": message_id,
        "room_id": chat_id,
        "sender_id": ai_user_id,
        "sender_username": ai_username,
        "sender_avatar_file_id": avatar,
        "content": content,
        "timestamp": timestamp,
        "reply_to_id": reply_to_message_id,
        "reply_preview": reply_preview
    });

    st.hub.broadcast_room(&room, &out);
    Ok(message_id)
}

pub fn spawn_channel_reply(st: AppState, chat_id: i64, human_user_id: i64, trigger_message_id: i64) {
    tokio::spawn(async move {
        if let Err(err) = maybe_reply_channel(st, chat_id, human_user_id, trigger_message_id).await {
            tracing::warn!(target: "ai", "Gemka III channel reply failed: {:#}", err);
        }
    });
}

async fn maybe_reply_channel(
    st: AppState,
    chat_id: i64,
    human_user_id: i64,
    trigger_message_id: i64,
) -> anyhow::Result<()> {
    let db = &st.db;
    let settings = get_settings(db).await;
    if !settings.enabled || !settings.channel_enabled || normalized_mode(&settings.mode) == "silent" {
        return Ok(());
    }

    let ai_user_id = ensure_ai_user(db, &settings).await?;
    if human_user_id == ai_user_id {
        return Ok(());
    }

    let meta = sqlx::query(
        r#"
        SELECT c.server_id,
               c.is_private,
               COALESCE(c.kind, 'text') AS kind,
               COALESCE(c.name, 'general') AS channel_name,
               COALESCE(s.name, 'server') AS server_name
        FROM chats c
        LEFT JOIN servers s ON s.id = c.server_id
        WHERE c.id = ?
        LIMIT 1
        "#,
    )
    .bind(chat_id)
    .fetch_optional(db)
    .await?;

    let Some(meta) = meta else { return Ok(()); };
    let server_id: Option<i64> = meta.try_get("server_id").ok();
    let Some(server_id) = server_id.filter(|id| *id > 0) else { return Ok(()); };
    let is_private: i64 = meta.get("is_private");
    let kind: String = meta.get("kind");
    if is_private != 0 || kind != "text" {
        return Ok(());
    }

    let trigger = sqlx::query("SELECT content, reply_to_message_id FROM messages WHERE id = ? AND chat_id = ? LIMIT 1")
        .bind(trigger_message_id)
        .bind(chat_id)
        .fetch_optional(db)
        .await?;

    let Some(trigger) = trigger else { return Ok(()); };
    let trigger_content: String = trigger.get("content");
    let reply_to_message_id: Option<i64> = trigger.try_get("reply_to_message_id").ok();

    if !should_reply_in_channel(db, &settings, &trigger_content, reply_to_message_id, ai_user_id).await {
        return Ok(());
    }

    let direct_trigger = direct_channel_trigger(db, &settings, &trigger_content, reply_to_message_id, ai_user_id).await;
    let channel_cooldown_seconds = if direct_trigger {
        settings.channel_cooldown_seconds.min(5)
    } else {
        settings.channel_cooldown_seconds.max(45)
    };
    if !cooldown_ok(db, chat_id, channel_cooldown_seconds).await {
        return Ok(());
    }

    let _ = sqlx::query("INSERT OR IGNORE INTO server_members(server_id, user_id, role) VALUES(?, ?, 'member')")
        .bind(server_id)
        .bind(ai_user_id)
        .execute(db)
        .await;

    reserve_cooldown(db, chat_id, trigger_message_id).await;

    let server_name: String = meta.get("server_name");
    let channel_name: String = meta.get("channel_name");
    let messages = build_channel_messages(db, chat_id, ai_user_id, &settings, &server_name, &channel_name).await?;
    let reply = clean_ai_reply(&call_lm_studio(&settings, messages).await?);
    if reply.is_empty() {
        if direct_trigger {
            penalize_ai_no_reply(db, 1).await;
        }
        return Ok(());
    }
    reward_ai_reply(db).await;

    insert_ai_message_and_broadcast(
        &st,
        chat_id,
        ai_user_id,
        &settings.user_name,
        &reply,
        Some(trigger_message_id),
        RoomId::Channel(chat_id),
    )
    .await?;

    Ok(())
}

async fn ensure_dm_chat_between(db: &SqlitePool, a: i64, b: i64) -> anyhow::Result<i64> {
    let (user_a, user_b) = if a < b { (a, b) } else { (b, a) };

    if let Some(chat_id) = sqlx::query_scalar::<_, i64>(
        "SELECT chat_id FROM dm_chats WHERE user_a = ? AND user_b = ? LIMIT 1",
    )
    .bind(user_a)
    .bind(user_b)
    .fetch_optional(db)
    .await?
    {
        return Ok(chat_id);
    }

    let now = auth::now_iso();
    let created = sqlx::query(
        "INSERT INTO chats(name, server_id, is_private, created_at) VALUES(NULL, NULL, 1, ?)",
    )
    .bind(&now)
    .execute(db)
    .await?;
    let chat_id = created.last_insert_rowid();

    sqlx::query("INSERT OR IGNORE INTO chat_participants(chat_id, user_id) VALUES(?, ?)")
        .bind(chat_id)
        .bind(user_a)
        .execute(db)
        .await?;
    sqlx::query("INSERT OR IGNORE INTO chat_participants(chat_id, user_id) VALUES(?, ?)")
        .bind(chat_id)
        .bind(user_b)
        .execute(db)
        .await?;
    sqlx::query("INSERT INTO dm_chats(chat_id, user_a, user_b, created_at) VALUES(?, ?, ?, ?)")
        .bind(chat_id)
        .bind(user_a)
        .bind(user_b)
        .bind(&now)
        .execute(db)
        .await?;

    Ok(chat_id)
}

pub fn spawn_start_dm_if_enabled(st: AppState, chat_id: i64, human_user_id: i64) {
    tokio::spawn(async move {
        if let Err(err) = maybe_start_dm(st, chat_id, human_user_id).await {
            tracing::warn!(target: "ai", "Gemka III start DM failed: {:#}", err);
        }
    });
}

async fn maybe_start_dm(st: AppState, chat_id: i64, human_user_id: i64) -> anyhow::Result<()> {
    let db = &st.db;
    let settings = get_settings(db).await;
    if !settings.enabled
        || !settings.dm_enabled
        || !settings.start_dm_enabled
        || normalized_mode(&settings.mode) == "silent"
    {
        return Ok(());
    }

    let ai_user_id = ensure_ai_user(db, &settings).await?;
    if human_user_id == ai_user_id {
        return Ok(());
    }
    if !dm_contains_pair(db, chat_id, human_user_id, ai_user_id).await? {
        return Ok(());
    }

    let existing_messages = sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM messages WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_one(db)
        .await
        .unwrap_or(0);
    if existing_messages != 0 {
        return Ok(());
    }

    if !cooldown_ok(db, chat_id, settings.dm_cooldown_seconds).await {
        return Ok(());
    }
    reserve_cooldown(db, chat_id, 0).await;

    let human_name = sqlx::query_scalar::<_, String>("SELECT username FROM users WHERE id = ? LIMIT 1")
        .bind(human_user_id)
        .fetch_optional(db)
        .await?
        .unwrap_or_else(|| "собеседник".to_string());

    let messages = vec![
        LmMessage {
            role: "system".to_string(),
            content: format!(
                "{}\n\nКонтекст: ты первой начинаешь личный чат с пользователем {}. Напиши одну короткую нейтральную фразу без восторга и навязчивости. Не говори, что ты ИИ.",
                settings.system_prompt,
                human_name
            ),
        },
        LmMessage {
            role: "user".to_string(),
            content: format!("Начни личный чат с {}.", human_name),
        },
    ];

    let reply = clean_ai_reply(&call_lm_studio(&settings, messages).await?);
    if reply.is_empty() {
        return Ok(());
    }

    insert_ai_message_and_broadcast(
        &st,
        chat_id,
        ai_user_id,
        &settings.user_name,
        &reply,
        None,
        RoomId::Channel(chat_id),
    )
    .await?;

    Ok(())
}

async fn decide_friend_request(
    db: &SqlitePool,
    settings: &AiSettings,
    sender_id: i64,
    receiver_id: i64,
) -> anyhow::Result<AiDecision> {
    let row = sqlx::query(
        r#"
        SELECT
            su.username AS sender_username,
            ru.username AS receiver_username,
            COALESCE((
                SELECT COUNT(1)
                FROM server_members sm1
                JOIN server_members sm2 ON sm2.server_id = sm1.server_id
                WHERE sm1.user_id = ? AND sm2.user_id = ?
            ), 0) AS shared_servers
        FROM users su
        JOIN users ru ON ru.id = ?
        WHERE su.id = ?
        LIMIT 1
        "#,
    )
    .bind(sender_id)
    .bind(receiver_id)
    .bind(receiver_id)
    .bind(sender_id)
    .fetch_optional(db)
    .await?;

    let Some(row) = row else {
        return Ok(AiDecision::Ignore);
    };

    let sender_username: String = row.get("sender_username");
    let receiver_username: String = row.get("receiver_username");
    let shared_servers: i64 = row.get("shared_servers");

    let messages = vec![
        LmMessage {
            role: "system".to_string(),
            content: format!(
                "{}\n\nТы принимаешь решение по заявке в друзья для аккаунта {}. Ответь только JSON без markdown: {{\"action\":\"accept\"}} или {{\"action\":\"decline\"}}. Не используй __NO_REPLY__. Не добавляй текст вне JSON.",
                settings.system_prompt,
                receiver_username
            ),
        },
        LmMessage {
            role: "user".to_string(),
            content: format!(
                "Заявка в друзья: отправитель='{}', получатель='{}', общих серверов={}. Реши, принимать ли заявку.",
                sender_username,
                receiver_username,
                shared_servers
            ),
        },
    ];

    let raw = call_lm_studio(settings, messages).await?;
    Ok(parse_ai_decision(&raw))
}

pub async fn auto_accept_friend_request_if_ai(st: AppState, sender_id: i64, receiver_id: i64) -> bool {
    let db = &st.db;
    let settings = get_settings(db).await;
    if !settings.enabled || !settings.accept_friend_requests {
        return false;
    }

    let Ok(ai_user_id) = ensure_ai_user(db, &settings).await else {
        return false;
    };
    if receiver_id != ai_user_id || sender_id == ai_user_id {
        return false;
    }

    let decision = match decide_friend_request(db, &settings, sender_id, receiver_id).await {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(target: "ai", "Gemka III friend request decision failed: {:#}", err);
            return false;
        }
    };

    let now = auth::now_iso();

    match decision {
        AiDecision::Accept => {
            let updated = sqlx::query(
                r#"
                UPDATE friend_requests
                SET status = 'accepted'
                WHERE sender_id = ? AND receiver_id = ? AND status = 'pending'
                "#,
            )
            .bind(sender_id)
            .bind(receiver_id)
            .execute(db)
            .await
            .map(|r| r.rows_affected())
            .unwrap_or(0);

            if updated == 0 {
                return false;
            }

            let _ = sqlx::query("INSERT OR IGNORE INTO friendships(user_id, friend_id, created_at) VALUES(?, ?, ?)")
                .bind(sender_id)
                .bind(receiver_id)
                .bind(&now)
                .execute(db)
                .await;

            let _ = sqlx::query("INSERT OR IGNORE INTO friendships(user_id, friend_id, created_at) VALUES(?, ?, ?)")
                .bind(receiver_id)
                .bind(sender_id)
                .bind(&now)
                .execute(db)
                .await;

            if settings.start_dm_enabled {
                if let Ok(chat_id) = ensure_dm_chat_between(db, sender_id, receiver_id).await {
                    spawn_start_dm_if_enabled(st.clone(), chat_id, sender_id);
                }
            }

            true
        }
        AiDecision::Decline => {
            let _ = sqlx::query(
                r#"
                UPDATE friend_requests
                SET status = 'rejected'
                WHERE sender_id = ? AND receiver_id = ? AND status = 'pending'
                "#,
            )
            .bind(sender_id)
            .bind(receiver_id)
            .execute(db)
            .await;
            false
        }
        AiDecision::Ignore => false,
    }
}

async fn decide_server_join_request(
    db: &SqlitePool,
    settings: &AiSettings,
    request_id: i64,
) -> anyhow::Result<AiDecision> {
    let row = sqlx::query(
        r#"
        SELECT
            r.id,
            r.server_id,
            r.requester_id,
            r.from_server_id,
            r.created_at,
            s.name AS server_name,
            u.username AS requester_username,
            fs.name AS from_server_name,
            COALESCE((
                SELECT COUNT(1)
                FROM server_members sm
                WHERE sm.user_id = r.requester_id
            ), 0) AS requester_servers_count
        FROM server_join_requests r
        JOIN servers s ON s.id = r.server_id
        JOIN users u ON u.id = r.requester_id
        LEFT JOIN servers fs ON fs.id = r.from_server_id
        WHERE r.id = ? AND r.status = 'pending'
        LIMIT 1
        "#,
    )
    .bind(request_id)
    .fetch_optional(db)
    .await?;

    let Some(row) = row else {
        return Ok(AiDecision::Ignore);
    };

    let server_name: String = row.get("server_name");
    let requester_username: String = row.get("requester_username");
    let from_server_name: Option<String> = row.try_get("from_server_name").ok();
    let requester_servers_count: i64 = row.get("requester_servers_count");

    let messages = vec![
        LmMessage {
            role: "system".to_string(),
            content: format!(
                "{}\n\nТы принимаешь решение по заявке на вступление в сервер LaBerry. Ответь только JSON без markdown: {{\"action\":\"accept\"}} или {{\"action\":\"decline\"}}. Не используй __NO_REPLY__. Не добавляй текст вне JSON.",
                settings.system_prompt
            ),
        },
        LmMessage {
            role: "user".to_string(),
            content: format!(
                "Заявка на сервер: сервер='{}', пользователь='{}', сервер-источник='{}', пользователь уже состоит в {} сервер(ах). Реши, принимать ли заявку.",
                server_name,
                requester_username,
                from_server_name.unwrap_or_else(|| "не указан".to_string()),
                requester_servers_count
            ),
        },
    ];

    let raw = call_lm_studio(settings, messages).await?;
    Ok(parse_ai_decision(&raw))
}

pub async fn auto_decide_server_join_request_if_ai(st: AppState, request_id: i64) -> Option<String> {
    let db = &st.db;
    let settings = get_settings(db).await;
    if !settings.enabled || !settings.accept_server_join_requests {
        return None;
    }

    let Ok(ai_user_id) = ensure_ai_user(db, &settings).await else {
        return None;
    };

    let row = sqlx::query(
        "SELECT server_id, requester_id FROM server_join_requests WHERE id = ? AND status = 'pending' LIMIT 1",
    )
    .bind(request_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;

    let server_id: i64 = row.get("server_id");
    let requester_id: i64 = row.get("requester_id");

    if requester_id == ai_user_id {
        return None;
    }

    let decision = match decide_server_join_request(db, &settings, request_id).await {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(target: "ai", "Gemka III server join request decision failed: {:#}", err);
            return None;
        }
    };

    let now = auth::now_iso();

    match decision {
        AiDecision::Accept => {
            let _ = sqlx::query("INSERT OR IGNORE INTO server_members(server_id, user_id) VALUES(?, ?)")
                .bind(server_id)
                .bind(requester_id)
                .execute(db)
                .await;

            let _ = sqlx::query(
                "UPDATE server_join_requests SET status = 'accepted', decided_at = ?, decided_by = ? WHERE id = ? AND status = 'pending'",
            )
            .bind(&now)
            .bind(ai_user_id)
            .bind(request_id)
            .execute(db)
            .await;

            Some("accepted".to_string())
        }
        AiDecision::Decline => {
            let _ = sqlx::query(
                "UPDATE server_join_requests SET status = 'rejected', decided_at = ?, decided_by = ? WHERE id = ? AND status = 'pending'",
            )
            .bind(&now)
            .bind(ai_user_id)
            .bind(request_id)
            .execute(db)
            .await;

            Some("rejected".to_string())
        }
        AiDecision::Ignore => None,
    }
}


pub async fn send_dev_link_to_user(st: AppState, user_id: i64, dev_url: &str) -> anyhow::Result<i64> {
    let db = &st.db;
    let dev_url = dev_url.trim();
    if dev_url.is_empty() {
        anyhow::bail!("dev-web ссылка пустая");
    }
    if !(dev_url.starts_with("http://") || dev_url.starts_with("https://")) {
        anyhow::bail!("dev-web ссылка должна начинаться с http:// или https://");
    }

    let settings = get_settings(db).await;
    let ai_user_id = ensure_ai_user(db, &settings).await?;
    if user_id == ai_user_id {
        anyhow::bail!("нельзя отправить dev-web ссылку самой Gemka");
    }

    let target_exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM users WHERE id = ? LIMIT 1")
        .bind(user_id)
        .fetch_one(db)
        .await
        .unwrap_or(0);
    if target_exists == 0 {
        anyhow::bail!("пользователь не найден");
    }

    let chat_id = ensure_dm_chat_between(db, ai_user_id, user_id).await?;
    let content = format!("Вот ссылка на dev-web: {}", dev_url);

    insert_ai_message_and_broadcast(
        &st,
        chat_id,
        ai_user_id,
        &settings.user_name,
        &content,
        None,
        RoomId::Channel(chat_id),
    )
    .await
}


pub async fn health_check(settings: &AiSettings) -> Result<String, (StatusCode, String)> {
    let messages = vec![
        LmMessage {
            role: "system".to_string(),
            content: "Отвечай только на русском, одной короткой фразой.".to_string(),
        },
        LmMessage {
            role: "user".to_string(),
            content: "Ты работаешь?".to_string(),
        },
    ];

    call_lm_studio(settings, messages)
        .await
        .map(|reply| clean_ai_reply(&reply))
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("LM Studio недоступна: {}", e)))
}
