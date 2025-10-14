use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json,
};
use bevy::prelude::*;
use bevy_defer::AsyncWorld;
use bevy_webgate::{serve_file, HttpErrorResponses, RouterAppExt, WebServerConfig};
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr};

/// A file server example that demonstrates how to serve static files
/// including HTML, CSS, JavaScript, images, and JSON data.
///
/// This example shows:
/// - Using bevy_webgate utilities for efficient file serving
/// - Proper MIME type handling with mime_guess crate
/// - Index file serving (index.html)
/// - Error handling for missing files
/// - Directory browsing protection
/// - API endpoints with standardized responses
///
/// Run with: `cargo run --example file_server`
/// Then visit: http://localhost:8080
fn main() {
    App::new()
        .add_plugins(MinimalPlugins)
        .add_plugins(bevy::log::LogPlugin::default())
        .insert_resource(WebServerConfig {
            ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 8080,
        })
        // Serve the main index page
        .route(
            "/",
            get(|| async { serve_file("examples/file_server_assets/index.html").await }),
        )
        // Serve static files using library utilities
        .route("/static/{*path}", get(serve_static_file))
        // Custom file serving example (for demonstration)
        .route("/custom/{*path}", get(serve_custom_file))
        // API endpoint to demonstrate JSON serving
        .route("/api/info", get(serve_api_info))
        // Fallback for any other routes
        .fallback(|| async {
            AsyncWorld
                .resource::<HttpErrorResponses>()
                .get(|errors| errors.create_response(StatusCode::NOT_FOUND))
                .unwrap_or_else(|_| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Service temporarily unavailable",
                    )
                        .into_response()
                })
        })
        .run();
}

async fn serve_static_file(Path(file_path): Path<String>) -> Response {
    serve_file(&format!("examples/file_server_assets/{}", file_path)).await
}

async fn serve_custom_file(Path(file_path): Path<String>) -> Response {
    serve_file(&format!("examples/file_server_assets/{}", file_path)).await
}

async fn serve_api_info() -> impl IntoResponse {
    let service_name = "Bevy WebGate File Server";
    let version = "0.3.0";
    let description = "A static file server built with Bevy and Axum";
    let endpoints = vec![
        ("/", "Main index page"),
        ("/static/*", "Static file serving (library utilities)"),
        ("/custom/*", "Custom file serving (library utilities)"),
        ("/api/info", "This API information"),
    ];
    let features = vec![
        "Static file serving with bevy_webgate utilities",
        "MIME type detection with mime_guess",
        "Security protection",
        "Caching headers",
        "JSON API endpoints",
        "Reusable utilities in library",
    ];

    let endpoints_obj: serde_json::Map<String, Value> = endpoints
        .into_iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();

    Json(json!({
        "service": service_name,
        "version": version,
        "description": description,
        "endpoints": endpoints_obj,
        "features": features
    }))
}
