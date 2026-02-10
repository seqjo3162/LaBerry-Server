use std::path::PathBuf;
use axum::response::Html;

/// Простой SPA-роутер — возвращает index.html для "/" и "/app"
pub async fn index() -> Html<String> {
    Html(std::fs::read_to_string(static_path("index.html")).unwrap_or_default())
}

pub async fn app() -> Html<String> {
    Html(std::fs::read_to_string(static_path("app.html")).unwrap_or_default())
}

/// Путь к статическим файлам
fn static_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("static")
        .join(file)
}
