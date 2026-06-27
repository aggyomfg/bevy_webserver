use std::net::IpAddr;
use thiserror::Error;

pub mod http;
pub use http::*;

pub type WebServerResult<T> = Result<T, WebServerError>;

#[derive(Debug, Error)]
pub enum WebServerError {
    #[error("Failed to bind to {ip}:{port}: {source}")]
    BindFailed {
        ip: IpAddr,
        port: u16,
        #[source]
        source: std::io::Error,
    },

    #[error("No server found listening on port {port}")]
    ServerNotFound { port: u16 },

    #[error("Server already running on port {port}")]
    ServerAlreadyRunning { port: u16 },

    #[error("IO operation '{operation}' failed: {source}")]
    IoError {
        operation: String,
        #[source]
        source: std::io::Error,
    },

    #[error("HTTP {status} error: {message}")]
    HttpError { status: u16, message: String },

    #[error("Configuration error in '{field}': {reason}")]
    ConfigError { field: String, reason: String },

    #[error("Operation '{operation}' timed out after {duration_ms}ms")]
    Timeout { operation: String, duration_ms: u64 },

    #[error("Authentication failed: {reason}")]
    AuthError { reason: String },

    #[error("Resource '{resource_type}' exhausted: {details}")]
    ResourceExhausted {
        resource_type: String,
        details: String,
    },
}

impl WebServerError {
    pub fn bind_failed(ip: IpAddr, port: u16, source: std::io::Error) -> Self {
        Self::BindFailed { ip, port, source }
    }

    pub fn server_not_found(port: u16) -> Self {
        Self::ServerNotFound { port }
    }

    pub fn server_already_running(port: u16) -> Self {
        Self::ServerAlreadyRunning { port }
    }

    pub fn io_error(operation: impl Into<String>, source: std::io::Error) -> Self {
        Self::IoError {
            operation: operation.into(),
            source,
        }
    }

    pub fn http_error(status: u16, message: impl Into<String>) -> Self {
        Self::HttpError {
            status,
            message: message.into(),
        }
    }

    pub fn config_error(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ConfigError {
            field: field.into(),
            reason: reason.into(),
        }
    }

    pub fn timeout(operation: impl Into<String>, duration_ms: u64) -> Self {
        Self::Timeout {
            operation: operation.into(),
            duration_ms,
        }
    }

    pub fn auth_error(reason: impl Into<String>) -> Self {
        Self::AuthError {
            reason: reason.into(),
        }
    }

    pub fn resource_exhausted(
        resource_type: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        Self::ResourceExhausted {
            resource_type: resource_type.into(),
            details: details.into(),
        }
    }
}

impl From<bevy_defer::AccessError> for WebServerError {
    fn from(error: bevy_defer::AccessError) -> Self {
        use bevy_defer::AccessError;

        match error {
            AccessError::EntityNotFound(entity) => Self::ResourceExhausted {
                resource_type: "entity".to_string(),
                details: format!("entity {:?} not found", entity),
            },
            AccessError::QueryConditionNotMet { entity, query } => Self::ResourceExhausted {
                resource_type: "query".to_string(),
                details: format!("query condition not met for entity {:?}: {}", entity, query),
            },
            AccessError::NoEntityFound { query } => Self::ResourceExhausted {
                resource_type: "entity".to_string(),
                details: format!("no entity found in query {}", query),
            },
            AccessError::TooManyEntities { query } => Self::ResourceExhausted {
                resource_type: "entity".to_string(),
                details: format!("too many entities in query {}", query),
            },
            AccessError::ChildNotFound { index } => Self::ResourceExhausted {
                resource_type: "child".to_string(),
                details: format!("child index {} missing", index),
            },
            AccessError::NamedChildNotFound => Self::ResourceExhausted {
                resource_type: "child".to_string(),
                details: "named child missing".to_string(),
            },
            AccessError::TypedChildNotFound { query } => Self::ResourceExhausted {
                resource_type: "child".to_string(),
                details: format!("child of type query {} missing", query),
            },
            AccessError::TypedParentNotFound { query } => Self::ResourceExhausted {
                resource_type: "parent".to_string(),
                details: format!("parent of type query {} missing", query),
            },
            AccessError::ComponentNotFound { entity, name } => Self::ResourceExhausted {
                resource_type: "component".to_string(),
                details: format!("component <{}> not found on entity {:?}", name, entity),
            },
            AccessError::ResourceNotFound { name } => Self::ResourceExhausted {
                resource_type: "resource".to_string(),
                details: format!("resource <{}> not found", name),
            },
            AccessError::AssetNotFound { name } => Self::ResourceExhausted {
                resource_type: "asset".to_string(),
                details: format!("asset <{}> not found", name),
            },
            AccessError::EventNotRegistered { name } => Self::ConfigError {
                field: "event_registration".to_string(),
                reason: format!("event <{}> not registered", name),
            },
            AccessError::DowncastFailed { name } => Self::ConfigError {
                field: "downcast".to_string(),
                reason: format!("downcasting {} failed", name),
            },
            AccessError::ScheduleNotFound => Self::ResourceExhausted {
                resource_type: "schedule".to_string(),
                details: "schedule not found".to_string(),
            },
            AccessError::SystemIdNotFound => Self::ResourceExhausted {
                resource_type: "system_id".to_string(),
                details: "SystemId not found".to_string(),
            },
            AccessError::NotInState { ty } => Self::ConfigError {
                field: "state".to_string(),
                reason: format!("not in state of type {}", ty),
            },
            AccessError::Custom(msg) => Self::ConfigError {
                field: "custom".to_string(),
                reason: msg.to_string(),
            },
            AccessError::TypedError { message, ty } => Self::ConfigError {
                field: ty.to_string(),
                reason: message.to_string(),
            },
            AccessError::ShouldNotHappen => {
                bevy_log::error!("bevy_defer reported an invariant violation: {error}");
                Self::ConfigError {
                    field: "bevy_defer".to_string(),
                    reason: error.to_string(),
                }
            }
            _ => {
                bevy_log::error!(
                    "unhandled non-exhaustive bevy_defer::AccessError variant: {error:?}"
                );
                Self::ConfigError {
                    field: "bevy_defer::AccessError".to_string(),
                    reason: format!("unhandled non-exhaustive AccessError variant: {error}"),
                }
            }
        }
    }
}
