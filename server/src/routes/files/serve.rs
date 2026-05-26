use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
};

use crate::server::AppState;

pub(crate) async fn get_file_link(
    State(st): State<AppState>,
    axum::extract::Path(file_id): axum::extract::Path<i64>,
) -> axum::response::Response {
    let _ = st;
    let _ = file_id;
    // TODO: implement file link endpoint
    (StatusCode::NOT_IMPLEMENTED, "link").into_response()
}

pub(crate) async fn get_preview(
    State(st): State<AppState>,
    axum::extract::Path(file_id): axum::extract::Path<i64>,
) -> axum::response::Response {
    let _ = st;
    let _ = file_id;
    // TODO: implement preview endpoint
    (StatusCode::NOT_IMPLEMENTED, "preview").into_response()
}

pub(crate) async fn get_archive(
    State(st): State<AppState>,
    axum::extract::Path(file_id): axum::extract::Path<i64>,
) -> axum::response::Response {
    let _ = st;
    let _ = file_id;
    // TODO: implement archive endpoint
    (StatusCode::NOT_IMPLEMENTED, "archive").into_response()
}

pub(crate) async fn get_file_raw(
    State(st): State<AppState>,
    axum::extract::Path(file_id): axum::extract::Path<i64>,
) -> axum::response::Response {
    let _ = st;
    let _ = file_id;
    // TODO: implement raw file endpoint
    (StatusCode::NOT_IMPLEMENTED, "raw").into_response()
}

pub(crate) async fn get_file(
    State(st): State<AppState>,
    axum::extract::Path(file_id): axum::extract::Path<i64>,
) -> axum::response::Response {
    let _ = st;
    let _ = file_id;
    // TODO: implement file download endpoint
    (StatusCode::NOT_IMPLEMENTED, "file").into_response()
}
