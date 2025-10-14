use async_io::Async;
use axum::Router;
use bevy_defer::{AccessError, AsyncExecutor, AsyncWorld};
use bevy_ecs::prelude::*;
use bevy_log::{debug, error, info, warn};
use hyper::server::conn::http1;
use hyper_util::service::TowerToHyperService;
use smol_hyper::rt::{FuturesIo, SmolTimer};
use std::net::{IpAddr, TcpListener};
use std::time::{Duration, Instant};

use crate::{WebServerError, WebServerResult};

const RETRY_DELAY_SECONDS: u64 = 10;
const MAX_RETRY_ATTEMPTS: usize = 100; // Allow up to 100 retry attempts

mod connection_tracker;
mod manager;
mod port;
mod status;
mod task_store;

pub use manager::WebServerManager;
pub use port::*;
pub use status::*;

pub(crate) use connection_tracker::*;
pub(crate) use task_store::*;

#[derive(Debug)]
pub struct WebServer {
    ip: IpAddr,
    port: WebPort,
    router: Router,
    status: ServerStatus,
    task_store: TaskStore,
    connection_tracker: ConnectionTracker,
    last_error: Option<String>,
    retry_count: usize,
    next_retry_time: Option<Instant>,
}

impl Clone for WebServer {
    fn clone(&self) -> Self {
        Self {
            ip: self.ip,
            port: self.port,
            router: self.router.clone(),
            status: self.status,
            task_store: Default::default(),
            connection_tracker: ConnectionTracker::default(),
            last_error: self.last_error.clone(),
            retry_count: self.retry_count,
            next_retry_time: self.next_retry_time,
        }
    }
}

impl WebServer {
    pub const ERROR_SLEEP_INTERVAL_MS: u64 = 100;
    pub fn new(ip: IpAddr, port: WebPort, router: Router) -> Self {
        Self {
            ip,
            port,
            router,
            status: ServerStatus::default(),
            task_store: Default::default(),
            connection_tracker: ConnectionTracker::default(),
            last_error: None,
            retry_count: 0,
            next_retry_time: None,
        }
    }

    pub fn ip(&self) -> IpAddr {
        self.ip
    }

    pub fn port(&self) -> WebPort {
        self.port
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    pub fn set_ip(&mut self, ip: IpAddr) {
        self.ip = ip;
    }

    pub fn set_port(&mut self, port: WebPort) {
        self.port = port;
    }

    pub fn router_mut(&mut self) -> &mut Router {
        &mut self.router
    }

    /// Immediately stop the server and cancel all tasks
    pub fn stop(&mut self) {
        self.task_store_mut().clear();
        self.set_status(ServerStatus::Stopped);
        debug!("Stopped web-server on {}:{}", self.ip, self.port);
    }

    /// Request graceful shutdown - stops accepting new connections but allows existing ones to complete
    pub fn graceful_shutdown(&mut self) {
        self.set_status(ServerStatus::Shutdown);
        debug!(
            "Requested graceful shutdown for web-server on {}:{}",
            self.ip, self.port
        );
    }

    /// Wait for all connections to complete with a timeout, then force stop if needed
    pub async fn graceful_shutdown_with_timeout(&mut self, timeout: Duration) -> bool {
        self.graceful_shutdown();

        let start_time = Instant::now(); // Wait for server task to complete (which happens when accept loop exits)
        while start_time.elapsed() < timeout {
            if !self.task_store().contains_key(&TaskType::Server) {
                break;
            }
            AsyncWorld
                .sleep(Duration::from_millis(WebServer::ERROR_SLEEP_INTERVAL_MS))
                .await;
        }

        // Count active connection tasks
        let mut active_connections = self.count_active_connections();
        info!(
            "Graceful shutdown: {} active connections remaining",
            active_connections
        );

        // Wait for connections to complete
        while active_connections > 0 && start_time.elapsed() < timeout {
            AsyncWorld
                .sleep(Duration::from_millis(WebServer::ERROR_SLEEP_INTERVAL_MS))
                .await;
            active_connections = self.count_active_connections();

            if active_connections > 0 {
                debug!(
                    "Graceful shutdown: {} connections still active, elapsed: {:?}",
                    active_connections,
                    start_time.elapsed()
                );
            }
        }

        let completed_gracefully = active_connections == 0;

        if completed_gracefully {
            info!(
                "Graceful shutdown completed for server on {}:{} in {:?}",
                self.ip,
                self.port,
                start_time.elapsed()
            );
        } else {
            warn!("Graceful shutdown timed out for server on {}:{} after {:?}, {} connections still active",
                  self.ip, self.port, start_time.elapsed(), active_connections);
        }

        // Force stop remaining tasks
        self.stop();

        completed_gracefully
    }

    pub fn shutdown_requested(&self) -> bool {
        self.status.shutdown_requested()
    }

    pub(crate) fn task_store(&self) -> &TaskStore {
        &self.task_store
    }

    pub(crate) fn task_store_mut(&mut self) -> &mut TaskStore {
        &mut self.task_store
    }

    pub(crate) fn next_connection_id(&self) -> usize {
        self.connection_tracker.total_connections()
    }

    pub(crate) fn count_active_connections(&self) -> usize {
        self.connection_tracker.active_connections()
    }

    pub(crate) fn new_connection(&self) -> ConnectionGuard {
        self.connection_tracker.new_connection()
    }

    pub(crate) fn status(&self) -> ServerStatus {
        self.status
    }

    pub(crate) fn set_status(&mut self, status: ServerStatus) {
        self.status = status;
    }

    pub(crate) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
    pub(crate) fn set_error(&mut self, error: String) {
        self.last_error = Some(error);
        self.status = ServerStatus::Failed;
    }

    pub(crate) fn clear_error(&mut self) {
        self.last_error = None;
    }

    /// Check if the server should retry starting (after retry delay has passed and max attempts not reached)
    pub(crate) fn should_retry(&self) -> bool {
        if self.retry_count >= MAX_RETRY_ATTEMPTS {
            return false;
        }

        if let Some(next_retry) = self.next_retry_time {
            Instant::now() >= next_retry
        } else {
            // No retry time set, can retry immediately
            true
        }
    }

    /// Set the server to retry state and schedule next retry attempt
    pub(crate) fn schedule_retry(&mut self) {
        if self.retry_count < MAX_RETRY_ATTEMPTS {
            self.retry_count += 1;
            self.next_retry_time = Some(Instant::now() + Duration::from_secs(RETRY_DELAY_SECONDS));
            self.status = ServerStatus::Retrying;

            info!(
                "Scheduling retry attempt {} for server on {}:{} in {} seconds",
                self.retry_count, self.ip, self.port, RETRY_DELAY_SECONDS
            );
        } else {
            warn!(
                "Max retry attempts ({}) reached for server on {}:{}. Setting to Failed state.",
                MAX_RETRY_ATTEMPTS, self.ip, self.port
            );

            self.set_error("Max retry attempts reached".to_string());
        }
    }

    /// Reset retry counter when server starts successfully
    pub(crate) fn reset_retry_count(&mut self) {
        self.retry_count = 0;
        self.next_retry_time = None;
    }

    /// Get server information (IP, port, and router) for a given port
    async fn server_info(port: WebPort) -> WebServerResult<(IpAddr, WebPort, Router)> {
        Ok(AsyncWorld
            .resource::<WebServerManager>()
            .get_mut(|manager| {
                let Some(server) = manager.get_server_mut(&port) else {
                    return Err(AccessError::Custom("No server found on port"));
                };

                let ip = server.ip();
                let port = server.port();
                let router = server.router().clone();

                Ok::<_, AccessError>((ip, port, router))
            })
            .map_err(|e| WebServerError::from(e))??)
    }

    async fn listen_accept_loop(ip: IpAddr, port: WebPort, router: Router) -> WebServerResult<()> {
        let async_executor = AsyncWorld
            .non_send_resource::<AsyncExecutor>()
            .get(|executor| executor.clone())?;

        let listener = Async::<TcpListener>::bind((ip, port)).map_err(|e| {
            error!("Failed to bind server on {}:{}: {}", ip, port, e);
            WebServerError::bind_failed(ip, port, e)
        })?;
        debug!("Successfully bound to {}:{}", ip, port);

        // Update server status to Running after successful bind
        AsyncWorld
            .resource::<WebServerManager>()
            .get_mut(|manager| {
                if let Some(server) = manager.get_server_mut(&port) {
                    server.set_status(ServerStatus::Running);
                    server.reset_retry_count();
                }
                Ok::<(), AccessError>(())
            })??;

        info!("Web server listening on {}:{}", ip, port);

        let service = TowerToHyperService::new(router);

        loop {
            // Check if shutdown is requested before accepting new connections
            let shutdown_requested =
                AsyncWorld.resource::<WebServerManager>().get(|manager| {
                    Ok::<bool, AccessError>(
                        manager
                            .get_server(&port)
                            .map(|server| server.shutdown_requested())
                            .unwrap_or(false),
                    )
                })??;

            if shutdown_requested {
                info!(
                    "Shutdown requested for server on port {}, stopping accept loop",
                    port
                );
                return Ok(());
            }

            let accept_result = listener.accept().await;

            match accept_result {
                Ok((client, _sock_addr)) => {
                    let connection_id =
                        AsyncWorld.resource::<WebServerManager>().get(|manager| {
                            manager
                                .get_server(&port)
                                .map(|server| server.next_connection_id())
                                .ok_or(AccessError::Custom("No server found on port"))
                        })??;

                    // Connection handling task
                    let connection_task = async_executor.spawn_task({
                        let service = service.clone();

                        let port = port;

                        async move {
                            let start_time = Instant::now();

                            // Get connection guard to track connection counter - the guard will automatically
                            // decrement the counter when dropped
                            AsyncWorld.resource::<WebServerManager>().get(|manager| {
                                manager
                                    .get_server(&port)
                                    .ok_or(AccessError::Custom("No server found on port"))
                                    .map(|server| server.new_connection())
                            })??;

                            let connection = http1::Builder::new()
                                .timer(SmolTimer::new())
                                .serve_connection(FuturesIo::new(client), service);

                            let result = connection.await;
                            let duration = start_time.elapsed();

                            match result {
                                Ok(_) => {
                                    debug!(
                                        "Connection {} completed in {:?}",
                                        connection_id, duration
                                    );
                                }
                                Err(err) => {
                                    let err_msg = err.to_string();
                                    if err_msg.contains("timeout") || err_msg.contains("incomplete")
                                    {
                                        debug!(
                                            "Connection {} timeout after {:?}: {}",
                                            connection_id, duration, err
                                        );
                                    } else {
                                        error!(
                                            "Connection {} error after {:?}: {}",
                                            connection_id, duration, err
                                        );
                                    }
                                }
                            }

                            // Cleanup from TaskStore
                            AsyncWorld
                                .resource::<WebServerManager>()
                                .get_mut(|manager| {
                                    if let Some(server) = manager.get_server_mut(&port) {
                                        let task_type = TaskType::Connection(connection_id);
                                        server.task_store_mut().remove(&task_type);
                                    }
                                    Ok(())
                                })??;

                            Ok(())
                        }
                    });

                    // Add task to TaskStore
                    AsyncWorld
                        .resource::<WebServerManager>()
                        .get_mut(|manager| {
                            let Some(server) = manager.get_server_mut(&port) else {
                                return Err(AccessError::Custom("No server found on port"));
                            };
                            let task_type = TaskType::Connection(connection_id);
                            server.task_store_mut().insert(task_type, connection_task);

                            Ok(())
                        })??;
                }

                Err(e) => {
                    error!("Error accepting connection on {}:{}: {}", ip, port, e);
                    AsyncWorld
                        .sleep(Duration::from_millis(WebServer::ERROR_SLEEP_INTERVAL_MS))
                        .await;
                }
            }
            AsyncWorld.yield_now().await;
        }
    }

    async fn run(port: WebPort) -> WebServerResult<()> {
        let (ip, port, router) = Self::server_info(port).await?;

        // Double-check port availability before binding in the retry logic
        if let Err(test_error) = crate::server::WebServerManager::test_bind(ip, port) {
            error!(
                "Port availability test failed before bind on {}:{}: {}",
                ip, port, test_error
            );
            return Err(test_error);
        }

        Self::listen_accept_loop(ip, port, router).await?;

        Ok(())
    }
}

/// Legacy configuration for a web server, used to initialize the `WebServerManager` resource
///  for backwards compatibility with older versions of the library.
#[derive(Clone, Debug, PartialEq, Resource)]
pub struct WebServerConfig {
    pub ip: IpAddr,
    pub port: WebPort,
}

impl Default for WebServerConfig {
    fn default() -> Self {
        Self {
            ip: crate::DEFAULT_IP,
            port: crate::DEFAULT_PORT,
        }
    }
}

impl From<WebServerConfig> for WebServerManager {
    fn from(config: WebServerConfig) -> Self {
        debug!(
            "Converting WebServerConfig to WebServerManager: {}:{}",
            config.ip, config.port
        );
        let mut servers = WebServerManager::default();
        if let Err(err) = servers.add_server(WebServer::new(config.ip, config.port, Router::new()))
        {
            error!(
                "Failed to add server on {}:{}: {}",
                config.ip, config.port, err
            );
        } else {
            debug!("Successfully added server on {}:{}", config.ip, config.port);
        }
        servers
    }
}
