#![allow(dead_code)]

// =======================
// CRATE MODULES
// =======================

pub mod auth;
pub mod db;
pub mod ws;
pub mod server;
pub mod routes;
pub mod middleware;
pub mod api_error;
#[cfg(target_os = "android")]
mod jni_bridge;

// =======================
// NOTE
// =======================
//
// Android JNI entrypoint intentionally NOT defined here.
//
// Android must use:
//   jni_bridge.rs
//   Java_com_laberry_app_NativeServer_*
//
// This avoids:
// - double server startup
// - multiple Tokio runtimes
// - port conflicts
// - broken shutdown logic
//
// lib.rs is now a pure crate root.
