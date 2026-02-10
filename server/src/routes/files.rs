use axum::{
    extract::{Multipart, Path, State},
    http::{header, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use std::path::PathBuf;
use tokio::{fs, fs::File, io::AsyncWriteExt};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::auth;
use crate::middleware::auth_guard::AuthUser;
use crate::server::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(upload_file))
        .route("/:file_id", get(get_file))
}

async fn upload_file(
    State(st): State<AppState>,
    me: AuthUser,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let storage_dir = PathBuf::from("storage/files");
    if fs::create_dir_all(&storage_dir).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let mut chat_id: Option<i64> = None;
    let mut original_name = "file.bin".to_string();
    let mut mime_type = "application/octet-stream".to_string();
    let mut saved_path: Option<PathBuf> = None;
    let mut file_size: i64 = 0;

    while let Ok(Some(mut field)) = multipart.next_field().await {
        match field.name().unwrap_or("") {
            "chat_id" => {
                if let Ok(txt) = field.text().await {
                    chat_id = txt.parse::<i64>().ok();
                }
            }

            "file" => {
                original_name = field.file_name().unwrap_or("file").to_string();
                mime_type = field
                    .content_type()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());

                let ext = std::path::Path::new(&original_name)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");

                let filename = if ext.is_empty() {
                    Uuid::new_v4().to_string()
                } else {
                    format!("{}.{}", Uuid::new_v4(), ext)
                };

                let path = storage_dir.join(&filename);
                let mut file = match fs::File::create(&path).await {
                    Ok(f) => f,
                    Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                };

                while let Some(chunk) = field.chunk().await.unwrap_or(None) {
                    file_size += chunk.len() as i64;
                    if file_size > 50 * 1024 * 1024 {
                        let _ = fs::remove_file(&path).await;
                        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
                    }
                    if file.write_all(&chunk).await.is_err() {
                        let _ = fs::remove_file(&path).await;
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                }

                saved_path = Some(path);
            }
            _ => {}
        }
    }

    let chat_id = match chat_id {
        Some(v) => v,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    let path = match saved_path {
        Some(p) => p,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    // Проверка членства в чате
    let member = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM chat_participants WHERE chat_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(chat_id)
    .bind(me.id)  // Исправлено: было me.user.id, теперь me.id
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !member {
        let _ = fs::remove_file(&path).await;
        return StatusCode::FORBIDDEN.into_response();
    }

    let created_at = auth::now_iso();

    let r = sqlx::query(
        r#"
        INSERT INTO files (
            filename, original_name, file_size, mime_type,
            storage_path, uploaded_by, chat_id, created_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(path.file_name().unwrap().to_str().unwrap())
    .bind(&original_name)
    .bind(file_size)
    .bind(&mime_type)
    .bind(path.to_string_lossy().to_string())
    .bind(me.id)  // Исправлено: было me.user.id, теперь me.id
    .bind(chat_id)
    .bind(&created_at)
    .execute(&st.db)
    .await;

    let Ok(r) = r else {
        let _ = fs::remove_file(&path).await;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let file_id = r.last_insert_rowid();

    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "id": file_id })),
    )
        .into_response()
}

pub async fn get_file(
    State(st): State<AppState>,
    Path(file_id): Path<i64>,
) -> impl IntoResponse {
    #[derive(sqlx::FromRow)]
    struct FileData {
        original_name: String,
        mime_type: String,
        storage_path: String,
        chat_id: i64,
    }
    
    let row: Option<FileData> = sqlx::query_as(
        "SELECT original_name, mime_type, storage_path, chat_id FROM files WHERE id = ?",
    )
    .bind(file_id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();

    let Some(f) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };
    
    let file = match File::open(PathBuf::from(f.storage_path)).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let body = axum::body::Body::from_stream(ReaderStream::new(file));

    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_str(&f.mime_type).unwrap(),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!("attachment; filename=\"{}\"", f.original_name))
                    .unwrap(),
            ),
        ],
        body,
    )
        .into_response()
}