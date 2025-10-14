use async_io::Async;
use axum::Router;
use bevy_defer::{AccessResult, AsyncCommandsExtension, AsyncExecutor, AsyncWorld};
use bevy_ecs::prelude::*;
use bevy_log::{debug, error, info, warn};
use std::{
    collections::HashMap,
    net::{IpAddr, TcpListener},
    time::Duration,
};

use super::{ServerStatus, TaskType};
use crate::{WebPort, WebServer, WebServerError, WebServerResult};

/// Resource to track running server tasks with shutdown capabilities
#[derive(Default, Resource)]
pub struct WebServerManager(HashMap<WebPort, WebServer>);

impl WebServerManager {
    const SHUTDOWN_CHECK_INTERVAL_MS: u64 = 100;

    pub fn cleanup_finished_tasks(mut manager: ResMut<Self>) {
        for (port, server) in manager.iter_mut() {
            let finished_count = server.task_store().finished_task_count();
            if finished_count > 0 {
                debug!(
                    "Cleaning up {} finished tasks on port {}",
                    finished_count, port
                );
            }

            server.task_store_mut().cleanup_finished_tasks();
        }
    }

    pub fn check_retry_servers(mut manager: ResMut<Self>) {
        let servers_to_retry: Vec<_> = manager
            .iter_mut()
            .filter(|(_, server)| {
                server.status() == crate::server::ServerStatus::Retrying && server.should_retry()
            })
            .map(|(port, _)| *port)
            .collect();

        if !servers_to_retry.is_empty() {
            debug!("Found {} servers ready to retry", servers_to_retry.len());
            // Trigger the WebServerManager::changed system by marking the resource as changed
            manager.set_changed();
        }
    }

    pub fn changed(mut manager: ResMut<Self>, async_executor: NonSend<AsyncExecutor>) {
        if !manager.is_changed() {
            return;
        }

        let servers_to_start: Vec<_> = manager
            .iter_mut()
            .filter(|(_, server)| server.status().can_start())
            .filter_map(|(port, server)| {
                if !server.task_store().contains_key(&TaskType::Server) {
                    // Check if this is a retry attempt and if it's time to retry
                    if server.status() == crate::server::ServerStatus::Retrying {
                        if server.should_retry() {
                            debug!("Retry time reached for server on port {}", port);
                            server.set_status(crate::server::ServerStatus::Starting);
                            Some(*port)
                        } else {
                            None // Not time to retry yet
                        }
                    } else {
                        server.set_status(crate::server::ServerStatus::Starting);
                        Some(*port)
                    }
                } else {
                    None
                }
            })
            .collect();

        for port in servers_to_start {
            debug!(" - Starting server on port {}", port);
            if let Err(err) = manager.start_server(&port, &async_executor) {
                error!("Failed to start server on port {}: {}", port, err);

                if let Some(server) = manager.0.get_mut(&port) {
                    server.schedule_retry();
                }
            }
        }
    }

    pub fn add_server(&mut self, server: WebServer) -> WebServerResult<()> {
        let port = server.port();
        let ip = server.ip();
        if self.0.contains_key(&port) {
            return Err(WebServerError::server_already_running(port));
        }

        // Try to test bind, but don't fail immediately - instead set server to retry mode
        match Self::test_bind(ip, port) {
            Ok(_) => {
                // Bind test passed, add server normally
                self.0.insert(port, server);
            }
            Err(bind_error) => {
                // Bind test failed, add server in retry mode
                warn!(
                    "Initial bind test failed for {}:{}, server will retry: {}",
                    ip, port, bind_error
                );
                let mut server = server;
                server.set_error(bind_error.to_string());
                server.schedule_retry();
                self.0.insert(port, server);
            }
        }

        Ok(())
    }

    pub fn remove_server(&mut self, port: &WebPort) {
        if let Some(mut server) = self.0.remove(port) {
            server.stop();
        }
    }

    pub fn stop_server(&mut self, port: &WebPort) {
        if let Some(server) = self.0.get_mut(port) {
            server.stop();
        }
    }

    /// Get the last error for a server, if any
    pub fn server_error(&self, port: &WebPort) -> Option<&str> {
        self.0.get(port).and_then(|server| server.last_error())
    }

    /// Check if a server has failed to start
    pub fn server_failed(&self, port: &WebPort) -> bool {
        self.0
            .get(port)
            .map(|server| server.status() == ServerStatus::Failed)
            .unwrap_or(false)
    }

    /// Get all servers with their status and any errors
    pub fn server_status_report(&self) -> Vec<(WebPort, ServerStatus, Option<String>)> {
        self.0
            .iter()
            .map(|(port, server)| {
                (
                    *port,
                    server.status(),
                    server.last_error().map(|s| s.to_string()),
                )
            })
            .collect()
    }

    pub fn stop_all(&mut self) {
        for (_, server) in self.0.iter_mut() {
            server.stop();
        }
        self.0.clear();
    }

    /// Request graceful shutdown for a specific server
    pub fn graceful_shutdown(&mut self, port: &WebPort) {
        if let Some(server) = self.0.get_mut(port) {
            // Only transition to Shutdown if not already ShuttingDown
            if server.status() != ServerStatus::ShuttingDown {
                server.graceful_shutdown();
            }
        }
    }

    /// Request graceful shutdown for a specific server with timeout (spawns async task)
    /// This method can be called from Bevy systems and will handle the shutdown internally
    pub fn graceful_shutdown_with_timeout(
        &mut self,
        port: &WebPort,
        timeout: Duration,
        commands: &mut Commands,
    ) {
        if !self.0.contains_key(port) {
            warn!("Cannot shutdown server on port {}: server not found", port);
            return;
        }

        info!(
            "Initiating graceful shutdown for server on port {} with timeout {:?}",
            port, timeout
        );

        // Set status to ShuttingDown to prevent restart
        if let Some(server) = self.0.get_mut(port) {
            server.set_status(ServerStatus::ShuttingDown);
        }

        // Request the graceful shutdown immediately
        self.graceful_shutdown(&port);

        let port = *port;

        // Spawn a task to monitor and enforce the timeout
        commands.spawn_task(async move || Self::shutdown_server(port, timeout).await);
    }

    pub async fn graceful_shutdown_server(&mut self, port: &WebPort, timeout: Duration) -> bool {
        if let Some(server) = self.0.get_mut(port) {
            server.graceful_shutdown_with_timeout(timeout).await
        } else {
            false
        }
    }

    pub async fn graceful_shutdown_all(&mut self, timeout: Duration) -> HashMap<WebPort, bool> {
        let mut results = HashMap::new();
        let ports: Vec<WebPort> = self.0.keys().copied().collect();

        for port in &ports {
            if let Some(server) = self.0.get_mut(port) {
                server.graceful_shutdown();
            }
        }

        for port in ports {
            if let Some(server) = self.0.get_mut(&port) {
                let completed_gracefully = server.graceful_shutdown_with_timeout(timeout).await;
                results.insert(port, completed_gracefully);
            }
        }

        self.0.clear();
        results
    }

    pub fn has_server(&self, port: &WebPort) -> bool {
        self.0.contains_key(port)
    }

    pub fn ports(&self) -> Vec<WebPort> {
        self.0.keys().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn shutdown_requested(&self, port: &WebPort) -> bool {
        self.0
            .get(port)
            .map(|server| server.shutdown_requested())
            .unwrap_or(false)
    }

    pub fn active_connections(&self, port: &WebPort) -> usize {
        self.0
            .get(port)
            .map(|server| server.count_active_connections())
            .unwrap_or(0)
    }

    pub fn shutdown_status(&self) -> HashMap<WebPort, (bool, usize)> {
        self.0
            .iter()
            .map(|(port, server)| {
                let shutdown_requested = server.shutdown_requested();
                let active_connections = server.count_active_connections();
                (*port, (shutdown_requested, active_connections))
            })
            .collect()
    }

    pub fn router(&self, port: &WebPort) -> Option<&Router> {
        self.0.get(port).map(|server| server.router())
    }

    pub fn router_mut(&mut self, port: &WebPort) -> Option<&mut Router> {
        self.0.get_mut(port).map(|server| server.router_mut())
    }

    pub fn set_router(&mut self, port: &WebPort, router: Router) {
        if let Some(server) = self.0.get_mut(port) {
            *server.router_mut() = router;
        } else {
            error!("No server found on port {}", port);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&WebPort, &WebServer)> {
        self.0.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&WebPort, &mut WebServer)> {
        self.0.iter_mut()
    }

    pub(crate) fn get_server(&self, port: &WebPort) -> Option<&WebServer> {
        self.0.get(port)
    }
    pub(crate) fn get_server_mut(&mut self, port: &WebPort) -> Option<&mut WebServer> {
        self.0.get_mut(port)
    }

    pub fn start_server(
        &mut self,
        port: &WebPort,
        executor: &AsyncExecutor,
    ) -> WebServerResult<()> {
        let port = *port;

        if let Some(server) = self.0.get(&port) {
            if server.task_store().contains_key(&TaskType::Server) {
                debug!("Server on port {} already has a running task", port);
                return Err(WebServerError::server_already_running(port));
            }
        }

        let server = self
            .0
            .get_mut(&port)
            .ok_or_else(|| WebServerError::server_not_found(port))?;

        // Clear any previous errors and set status to Starting
        server.clear_error();
        server.set_status(ServerStatus::Starting);

        // We'll handle bind errors in the async task
        let server_task = executor.spawn_task({
            async move {
                if let Err(err) = WebServer::run(port).await {
                    error!("bevy_webserver on port {} failed with: {}", port, err);
                    // Store error in server and schedule retry
                    let _ = AsyncWorld
                        .resource::<WebServerManager>()
                        .get_mut(|manager| {
                            if let Some(server) = manager.get_server_mut(&port) {
                                server.set_error(err.to_string());
                                // Check if this is a bind error and schedule retry
                                if err.to_string().contains("already in use")
                                    || err.to_string().contains("bind")
                                {
                                    server.schedule_retry();
                                } else {
                                    // For non-bind errors, set to Failed without retry
                                    server.set_status(crate::server::ServerStatus::Failed);
                                }
                            }
                            Ok::<(), bevy_defer::AccessError>(())
                        });
                } else {
                    // Server started successfully, update status
                    let _ = AsyncWorld
                        .resource::<WebServerManager>()
                        .get_mut(|manager| {
                            if let Some(server) = manager.get_server_mut(&port) {
                                server.set_status(crate::server::ServerStatus::Running);
                            }
                            Ok::<(), bevy_defer::AccessError>(())
                        });
                }
                Ok(())
            }
        });

        let server = self
            .0
            .get_mut(&port)
            .ok_or_else(|| WebServerError::server_not_found(port))?;

        server
            .task_store_mut()
            .insert(TaskType::Server, server_task);

        Ok(())
    }

    /// Shutdown server with timeout, monitoring active connections
    async fn shutdown_server(port: WebPort, timeout: Duration) -> AccessResult {
        let start_time = std::time::Instant::now();
        loop {
            let shutdown_result = AsyncWorld.run(|world| {
                let manager = world.resource::<WebServerManager>();

                // Check if server still exists and has active connections
                if manager.has_server(&port) {
                    let active_connections = manager.active_connections(&port);
                    let elapsed = start_time.elapsed();

                    if elapsed >= timeout {
                        // Timeout reached
                        if active_connections > 0 {
                            warn!("⚠️ Graceful shutdown timeout reached after {:?}, {} connections still active", elapsed, active_connections);
                        } else {
                            info!("✅ Server on port {} shutdown gracefully within timeout", port);
                        }
                        Some(()) // Shutdown complete
                    } else if active_connections == 0 {
                        info!("✅ Server on port {} shutdown gracefully in {:?}", port, elapsed);
                        Some(()) // Shutdown complete
                    } else {
                        // Still have active connections, continue monitoring
                        None
                    }
                } else {
                    // Server no longer exists, shutdown complete
                    info!("✅ Server on port {} shutdown completed", port);
                    Some(()) // Shutdown complete
                }
            });

            if shutdown_result.is_some() {
                break;
            }

            // Sleep for a short time before checking again
            AsyncWorld
                .sleep(Duration::from_millis(Self::SHUTDOWN_CHECK_INTERVAL_MS))
                .await;
        }

        // Force stop and remove server after timeout
        AsyncWorld.run(|world| {
            let mut manager = world.resource_mut::<WebServerManager>();
            manager.remove_server(&port);
        });
        Ok(())
    }

    /// Wait for server to start and return result
    /// This method will block until the server either starts successfully or fails
    pub async fn wait_for_server_start(
        &self,
        port: &WebPort,
        timeout: Duration,
    ) -> WebServerResult<()> {
        let start_time = std::time::Instant::now();

        loop {
            if let Some(server) = self.0.get(port) {
                match server.status() {
                    ServerStatus::Running => return Ok(()),
                    ServerStatus::Failed => {
                        if let Some(error) = server.last_error() {
                            return Err(WebServerError::io_error(
                                format!("server startup on port {}", port),
                                std::io::Error::new(std::io::ErrorKind::Other, error.to_string()),
                            ));
                        } else {
                            return Err(WebServerError::io_error(
                                format!("server startup on port {}", port),
                                std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    "Server failed to start",
                                ),
                            ));
                        }
                    }
                    ServerStatus::Starting => {
                        // Still starting, continue waiting
                        if start_time.elapsed() > timeout {
                            return Err(WebServerError::timeout(
                                format!("starting server on port {}", port),
                                timeout.as_millis() as u64,
                            ));
                        }
                    }
                    _ => {
                        return Err(WebServerError::config_error(
                            "server_status",
                            format!(
                                "Server on port {} has unexpected status: {:?}",
                                port,
                                server.status()
                            ),
                        ));
                    }
                }
            } else {
                return Err(WebServerError::server_not_found(*port));
            }

            // Sleep for a short time before checking again
            AsyncWorld.sleep(Duration::from_millis(10)).await;
        }
    }

    /// Test if we can bind to a specific IP and port using reliable OS-level port checking
    pub fn test_bind(ip: IpAddr, port: WebPort) -> WebServerResult<()> {
        debug!("Testing bind on {}:{}", ip, port);

        // Check if port is free by attempting to bind to 0.0.0.0
        match TcpListener::bind(("0.0.0.0", port)) {
            Ok(listener) => {
                // Successfully bound, so port is free
                drop(listener);
            }
            Err(_) => {
                // Could not bind to 0.0.0.0, port is definitely occupied
                let error_msg = format!("Port {} is already in use", port);
                error!("{}:{}: {}", ip, port, error_msg);
                return Err(WebServerError::bind_failed(
                    ip,
                    port,
                    std::io::Error::new(std::io::ErrorKind::AddrInUse, error_msg),
                ));
            }
        }

        // If the port appears free, also verify with the async_io bind attempt
        // This catches edge cases where the port becomes occupied between checks
        let listener = Async::<TcpListener>::bind((ip, port)).map_err(|e| {
            error!("Test bind failed on {}:{}: {}", ip, port, e);
            WebServerError::bind_failed(ip, port, e)
        })?;

        drop(listener);
        Ok(())
    }
}
