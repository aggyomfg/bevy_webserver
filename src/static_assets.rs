use crate::error::HttpErrorResponses;
use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bevy_app::{App, Plugin};
use bevy_defer::AsyncWorld;
use bevy_derive::{Deref, DerefMut};
use bevy_ecs::prelude::*;
use bevy_log::error;
use std::{collections::HashSet, fs};

pub struct WebStaticAssetsPlugin;

impl Plugin for WebStaticAssetsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WebStaticFileExtensions>();

        app.add_plugins(crate::error::HttpErrorPlugin);
    }
}

#[derive(Clone, Deref, DerefMut, Resource)]
pub struct WebStaticFileExtensions {
    extensions: HashSet<String>,
}

impl WebStaticFileExtensions {
    const DEFAULT_EXTENSIONS: [&'static str; 15] = [
        "css", "js", "png", "jpg", "jpeg", "gif", "svg", "ico", "woff", "woff2", "ttf", "eot",
        "pdf", "webp", "avif",
    ];
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_extensions<I, S>(extensions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            extensions: extensions.into_iter().map(|s| s.into()).collect(),
        }
    }

    pub fn add_extension<S: Into<String>>(&mut self, extension: S) {
        self.extensions.insert(extension.into());
    }

    pub fn remove_extension(&mut self, extension: &str) {
        self.extensions.remove(extension);
    }

    pub fn contains(&self, extension: &str) -> bool {
        self.extensions.contains(extension)
    }

    pub fn clear(&mut self) {
        self.extensions.clear();
    }

    pub async fn is_static_asset(file_path: &str) -> bool {
        if let Some(extension) = std::path::Path::new(file_path)
            .extension()
            .and_then(|ext| ext.to_str())
        {
            // Try to get the static extensions from the resource
            match AsyncWorld
                .resource::<WebStaticFileExtensions>()
                .get(|extensions| extensions.contains(extension))
            {
                Ok(is_static) => is_static,
                Err(_) => {
                    // Fallback to default extensions if resource is not available
                    Self::DEFAULT_EXTENSIONS.contains(&extension)
                }
            }
        } else {
            false
        }
    }
}

impl Default for WebStaticFileExtensions {
    fn default() -> Self {
        Self {
            extensions: Self::DEFAULT_EXTENSIONS
                .iter()
                .map(|&s| s.to_string())
                .collect(),
        }
    }
}

pub async fn serve_file(file_path: &str) -> Response {
    let safe_path = crate::utils::sanitize_path(file_path);

    match fs::read(&safe_path) {
        Ok(contents) => {
            let mut headers = HeaderMap::new();

            let mime_type = mime_guess::from_path(&safe_path)
                .first_or_octet_stream()
                .to_string();

            headers.insert(header::CONTENT_TYPE, mime_type.parse().unwrap());

            // Add cache control for static assets
            if WebStaticFileExtensions::is_static_asset(&safe_path).await {
                headers.insert(
                    header::CACHE_CONTROL,
                    "public, max-age=3600".parse().unwrap(),
                );
            }

            (headers, contents).into_response()
        }
        Err(_) => {
            bevy_log::info!("File not found: {}", safe_path);

            // Try to get custom 404 response
            match AsyncWorld
                .resource::<HttpErrorResponses>()
                .get(|responses| responses.create_response(StatusCode::NOT_FOUND))
            {
                Ok(response) => response,
                Err(_) => {
                    error!("Failed to create 404 response, using default");
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Service temporarily unavailable",
                    )
                        .into_response()
                }
            }
        }
    }
}
