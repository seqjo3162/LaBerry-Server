// ======================================================
// 🔐 Two-Factor Authentication (2FA) Routes
// ======================================================
// Extended 2FA management: backup codes, setup, verification

use axum::{
    extract::{State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};

use serde::{Deserialize, Serialize};
use sqlx::Row;
use crate::{auth, server::AppState, api_error::ApiError};
use crate::middleware::auth_guard::AuthUser;

// ===========================
// Structures
// ===========================

#[derive(Serialize)]
pub struct BackupCode {
    pub code: String,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct BackupCodesResponse {
    pub backup_codes: Vec<BackupCode>,
    pub created_at: String,
    pub note: String,
}

#[derive(Deserialize)]
pub struct VerifyBackupCodeBody {
    pub backup_code: String,
}

#[derive(Serialize)]
pub struct TwoFactorStatusResponse {
    pub is_enabled: bool,
    pub backup_codes_count: i64,
    pub created_at: Option<String>,
}

// ===========================
// Router
// ===========================

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/status", get(get_2fa_status))
        .route("/setup", post(setup_2fa))
        .route("/disable", post(disable_2fa))
        .route("/backup-codes/generate", post(generate_backup_codes))
        .route("/backup-codes/list", get(list_backup_codes))
        .route("/backup-codes/verify", post(verify_backup_code))
}

// ===========================
// Get 2FA Status
// ===========================

async fn get_2fa_status(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let r = sqlx::query(
        r#"SELECT is_2fa_enabled FROM users WHERE id = ?"#,
    )
    .bind(me.id)
    .fetch_optional(db)
    .await;

    let is_enabled = match r {
        Ok(Some(row)) => row.get::<i64, _>("is_2fa_enabled") != 0,
        _ => false,
    };

    // Count remaining backup codes
    let backup_count = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*) FROM two_factor_backup_codes WHERE user_id = ? AND is_used = 0"#,
    )
    .bind(me.id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .unwrap_or(0);

    (StatusCode::OK, Json(TwoFactorStatusResponse {
        is_enabled,
        backup_codes_count: backup_count,
        created_at: None,
    })).into_response()
}

// ===========================
// Setup 2FA
// ===========================

async fn setup_2fa(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let r = sqlx::query(
        r#"SELECT is_2fa_enabled FROM users WHERE id = ?"#,
    )
    .bind(me.id)
    .fetch_optional(db)
    .await;

    if let Ok(Some(_)) = r {
        // Generate and store initial backup codes
        let backup_codes = auth::generate_2fa_backup_codes();
        let now = auth::now_iso();

        for code in &backup_codes {
            let code_hash = auth::sha256_hex(code);
            let _ = sqlx::query(
                r#"
                INSERT INTO two_factor_backup_codes(user_id, code_hash, created_at)
                VALUES(?, ?, ?)
                "#,
            )
            .bind(me.id)
            .bind(&code_hash)
            .bind(&now)
            .execute(db)
            .await;
        }

        // Enable 2FA on user account
        let _ = sqlx::query(
            r#"UPDATE users SET is_2fa_enabled = 1 WHERE id = ?"#,
        )
        .bind(me.id)
        .execute(db)
        .await;

        return (StatusCode::OK, Json(BackupCodesResponse {
            backup_codes: backup_codes.into_iter().map(|code| BackupCode {
                code,
                created_at: now.clone(),
            }).collect(),
            created_at: now,
            note: "⚠️  Save these backup codes in a secure place. Each code can be used ONCE to recover your account if you lose access to your 2FA device.".to_string(),
        })).into_response();
    }

    StatusCode::BAD_REQUEST.into_response()
}

// ===========================
// Disable 2FA
// ===========================

async fn disable_2fa(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let result = sqlx::query(
        r#"UPDATE users SET is_2fa_enabled = 0 WHERE id = ?"#,
    )
    .bind(me.id)
    .execute(db)
    .await;

    // Also clear all backup codes
    let _ = sqlx::query(
        r#"DELETE FROM two_factor_backup_codes WHERE user_id = ?"#,
    )
    .bind(me.id)
    .execute(db)
    .await;

    match result {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// ===========================
// Generate New Backup Codes
// ===========================

async fn generate_backup_codes(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    // Check if 2FA is enabled
    let r = sqlx::query_scalar::<_, i64>(
        r#"SELECT is_2fa_enabled FROM users WHERE id = ?"#,
    )
    .bind(me.id)
    .fetch_optional(db)
    .await;

    if let Ok(Some(enabled)) = r {
        if enabled == 0 {
            return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "2FA not enabled"}))).into_response();
        }
    } else {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Clear old backup codes
    let _ = sqlx::query(
        r#"DELETE FROM two_factor_backup_codes WHERE user_id = ?"#,
    )
    .bind(me.id)
    .execute(db)
    .await;

    // Generate new codes
    let backup_codes = auth::generate_2fa_backup_codes();
    let now = auth::now_iso();

    for code in &backup_codes {
        let code_hash = auth::sha256_hex(code);
        let _ = sqlx::query(
            r#"
            INSERT INTO two_factor_backup_codes(user_id, code_hash, created_at)
            VALUES(?, ?, ?)
            "#,
        )
        .bind(me.id)
        .bind(&code_hash)
        .bind(&now)
        .execute(db)
        .await;
    }

    (StatusCode::OK, Json(BackupCodesResponse {
        backup_codes: backup_codes.into_iter().map(|code| BackupCode {
            code,
            created_at: now.clone(),
        }).collect(),
        created_at: now,
        note: "⚠️  Your previous backup codes have been invalidated. Save these new codes securely.".to_string(),
    })).into_response()
}

// ===========================
// List Backup Codes
// ===========================

async fn list_backup_codes(
    State(st): State<AppState>,
    me: AuthUser,
) -> impl IntoResponse {
    let db = &st.db;

    let rows = sqlx::query(
        r#"
        SELECT created_at, is_used 
        FROM two_factor_backup_codes 
        WHERE user_id = ?
        ORDER BY created_at DESC
        "#,
    )
    .bind(me.id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let unused = rows.iter().filter(|r| r.get::<i64, _>("is_used") == 0).count();
    let total = rows.len();

    (StatusCode::OK, Json(serde_json::json!({
        "total": total,
        "unused": unused,
        "used": total - unused,
    }))).into_response()
}

// ===========================
// Verify Backup Code (for account recovery)
// ===========================

async fn verify_backup_code(
    State(st): State<AppState>,
    me: AuthUser,
    Json(body): Json<VerifyBackupCodeBody>,
) -> Result<impl IntoResponse, ApiError> {
    let db = &st.db;

    let code_hash = auth::sha256_hex(&body.backup_code);

    // Find and verify code
    let r = sqlx::query(
        r#"
        SELECT id, is_used FROM two_factor_backup_codes 
        WHERE user_id = ? AND code_hash = ?
        "#,
    )
    .bind(me.id)
    .bind(&code_hash)
    .fetch_optional(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?
    .ok_or(ApiError::Unauthorized("Invalid backup code"))?;

    if r.get::<i64, _>("is_used") != 0 {
        return Err(ApiError::Unauthorized("Backup code already used"));
    }

    // Mark as used
    let now = auth::now_iso();
    sqlx::query(
        r#"UPDATE two_factor_backup_codes SET is_used = 1, used_at = ? WHERE id = ?"#,
    )
    .bind(&now)
    .bind(r.get::<i64, _>("id"))
    .execute(db)
    .await
    .map_err(|_| ApiError::Internal("Database error"))?;

    Ok((StatusCode::OK, Json(serde_json::json!({
        "ok": true,
        "message": "Backup code verified"
    }))))
}
