use axum::handler::Handler;
use axum::response::IntoResponse;
use axum::routing::{MethodRouter, Route};
use axum::Router;
use bevy_app::App;
use bevy_ecs::world::Mut;
use std::convert::Infallible;
use std::net::IpAddr;
use tower::{Layer, Service};

use crate::{
    BevyWebServerPlugin, WebPort, WebServer, WebServerConfig, WebServerManager, WebServerResult,
    DEFAULT_IP, DEFAULT_PORT,
};

/// Extends Bevy App with multi-port web server capabilities and server management.
///
/// This trait provides methods for managing multiple web servers on different ports,
/// configuring port-specific routing, and managing server lifecycle.
///
/// # Examples
///
/// ```rust
/// use bevy::prelude::*;
/// use bevy_webgate::{WebServerAppExt, BevyWebServerPlugin};
/// use axum::routing::get;
///
/// let mut app = App::new();
/// app.add_plugins(MinimalPlugins)
///    .add_plugins(BevyWebServerPlugin);
///
/// // Multi-port usage
/// app.port_route(8081, "/admin", get(|| async { "Admin" }))
///    .port_route(8082, "/health", get(|| async { "OK" }));
///
/// // Custom IP binding
/// app.add_server("127.0.0.1".parse().unwrap(), 8083);
/// ```
pub trait WebServerAppExt {
    /// Add a server on specific IP and port
    fn add_server(&mut self, ip: IpAddr, port: WebPort) -> &mut Self;

    /// Update a server configuration at runtime
    fn update_server(
        &mut self,
        ip: IpAddr,
        port: WebPort,
        router: Router,
    ) -> WebServerResult<&mut Self>;

    /// Remove a server
    fn remove_server(&mut self, port: WebPort) -> WebServerResult<&mut Self>;

    /// Add a route to a specific port
    fn port_route(
        &mut self,
        port: WebPort,
        path: &str,
        method_router: MethodRouter<()>,
    ) -> &mut Self;

    /// Configure router for specific port
    fn port_router(&mut self, port: WebPort, router_fn: impl FnOnce(Router) -> Router)
        -> &mut Self;

    /// Add nested routes to a specific port
    fn port_nest(&mut self, port: WebPort, path: &str, router: Router<()>) -> &mut Self;

    /// Add a service to a specific port
    fn port_route_service<T>(&mut self, port: WebPort, path: &str, service: T) -> &mut Self
    where
        T: Service<axum::extract::Request, Error = Infallible> + Clone + Send + Sync + 'static,
        T::Response: IntoResponse,
        T::Future: Send + 'static;

    /// Merge another router into a specific port
    fn port_merge<R>(&mut self, port: WebPort, other: R) -> &mut Self
    where
        R: Into<Router<()>>;

    /// Add a layer to a specific port
    fn port_layer<L>(&mut self, port: WebPort, layer: L) -> &mut Self
    where
        L: Layer<Route> + Clone + Send + Sync + 'static,
        L::Service: Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as Service<axum::extract::Request>>::Response: IntoResponse + 'static,
        <L::Service as Service<axum::extract::Request>>::Error: Into<Infallible> + 'static,
        <L::Service as Service<axum::extract::Request>>::Future: Send + 'static;

    /// Add a fallback handler to a specific port
    fn port_fallback<H, T>(&mut self, port: WebPort, handler: H) -> &mut Self
    where
        H: Handler<T, ()>,
        T: 'static;

    /// Get information about running servers
    fn running_servers(&self) -> Vec<(WebPort, IpAddr)>;

    /// All configured ports
    fn routed_ports(&self) -> Vec<WebPort>;

    /// Get the number of configured servers
    fn server_count(&self) -> usize;
}

impl WebServerAppExt for App {
    fn add_server(&mut self, ip: IpAddr, port: WebPort) -> &mut Self {
        self.world_mut().init_resource::<WebServerManager>();
        if !self.is_plugin_added::<BevyWebServerPlugin>() {
            self.add_plugins(BevyWebServerPlugin);
        }

        self.world_mut()
            .resource_scope(|_world, mut manager: Mut<WebServerManager>| {
                let _ = manager.add_server(WebServer::new(ip, port, Router::new()));
            });
        self
    }

    fn update_server(
        &mut self,
        ip: IpAddr,
        port: WebPort,
        router: Router,
    ) -> WebServerResult<&mut Self> {
        self.world_mut().init_resource::<WebServerManager>();
        if !self.is_plugin_added::<BevyWebServerPlugin>() {
            self.add_plugins(BevyWebServerPlugin);
        }
        self.world_mut()
            .resource_scope(|_world, mut manager: Mut<WebServerManager>| {
                manager.add_server(WebServer::new(ip, port, router))
            })?;

        Ok(self)
    }

    fn remove_server(&mut self, port: WebPort) -> WebServerResult<&mut Self> {
        self.world_mut()
            .resource_scope(|_world, mut manager: Mut<WebServerManager>| {
                manager.remove_server(&port);
            });
        Ok(self)
    }

    fn port_route(
        &mut self,
        port: WebPort,
        path: &str,
        method_router: MethodRouter<()>,
    ) -> &mut Self {
        self.port_router(port, |router| router.route(path, method_router));
        self
    }

    fn port_router(
        &mut self,
        port: WebPort,
        router_fn: impl FnOnce(Router) -> Router,
    ) -> &mut Self {
        self.world_mut().init_resource::<WebServerManager>();
        if !self.is_plugin_added::<BevyWebServerPlugin>() {
            self.add_plugins(BevyWebServerPlugin);
        }

        self.world_mut()
            .resource_scope(|world, mut manager: Mut<WebServerManager>| {
                // Get default IP for new servers
                let default_ip = world
                    .get_resource::<WebServerConfig>()
                    .map_or(DEFAULT_IP, |config| config.ip);

                let existing_router = manager
                    .get_server(&port)
                    .map(|srv| srv.router().clone())
                    .unwrap_or_default();

                let new_router = router_fn(existing_router);
                if !manager.has_server(&port) {
                    let _ = manager.add_server(WebServer::new(default_ip, port, new_router));
                } else {
                    manager.set_router(&port, new_router);
                }
            });

        self
    }

    fn port_nest(&mut self, port: WebPort, path: &str, router: Router<()>) -> &mut Self {
        self.port_router(port, |r| r.nest(path, router))
    }

    fn port_route_service<T>(&mut self, port: WebPort, path: &str, service: T) -> &mut Self
    where
        T: Service<axum::extract::Request, Error = Infallible> + Clone + Send + Sync + 'static,
        T::Response: IntoResponse,
        T::Future: Send + 'static,
    {
        self.port_router(port, |r| r.route_service(path, service))
    }

    fn port_merge<R>(&mut self, port: WebPort, other: R) -> &mut Self
    where
        R: Into<Router<()>>,
    {
        self.port_router(port, |r| r.merge(other))
    }

    fn port_layer<L>(&mut self, port: WebPort, layer: L) -> &mut Self
    where
        L: Layer<Route> + Clone + Send + Sync + 'static,
        L::Service: Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as Service<axum::extract::Request>>::Response: IntoResponse + 'static,
        <L::Service as Service<axum::extract::Request>>::Error: Into<Infallible> + 'static,
        <L::Service as Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.port_router(port, |r| r.layer(layer))
    }
    fn port_fallback<H, T>(&mut self, port: WebPort, handler: H) -> &mut Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.port_router(port, |r| r.fallback(handler))
    }

    fn running_servers(&self) -> Vec<(WebPort, IpAddr)> {
        let running_servers = self.world().get_resource::<WebServerManager>();
        let manager = self.world().get_resource::<WebServerManager>();

        match (running_servers, manager) {
            (Some(servers), Some(manager)) => servers
                .ports()
                .into_iter()
                .filter_map(|port| manager.get_server(&port).map(|srv| (port, srv.ip())))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn routed_ports(&self) -> Vec<WebPort> {
        self.world()
            .get_resource::<WebServerManager>()
            .map(|manager| manager.ports())
            .unwrap_or_default()
    }

    fn server_count(&self) -> usize {
        self.world()
            .get_resource::<WebServerManager>()
            .map(|manager| manager.len())
            .unwrap_or(0)
    }
}

/// Turns a Bevy App into a web server application with routing capabilities.
/// This is legacy of older versions of Bevy Web Server, which used a single-port model.
///
/// This trait provides basic routing functionality for single-port web servers.
/// For multi-port capabilities, use `WebServerAppExt`.
///
/// # Examples
///
/// ```rust
/// use bevy::prelude::*;
/// use bevy_webgate::{RouterAppExt, BevyWebServerPlugin};
/// use axum::routing::get;
///
/// let mut app = App::new();
/// app.add_plugins(MinimalPlugins)
///    .add_plugins(BevyWebServerPlugin);
///
/// // Simple single-port usage
/// app.route("/", get(|| async { "Hello World" }))
///    .route("/api", get(|| async { "API" }));
/// ```
pub trait RouterAppExt {
    fn router(&mut self, router_fn: impl FnOnce(Router) -> Router);
    fn route(&mut self, path: &str, method_router: MethodRouter<()>) -> &mut Self;
    fn route_service<T>(&mut self, path: &str, service: T) -> &mut Self
    where
        T: Service<axum::extract::Request, Error = Infallible> + Clone + Send + Sync + 'static,
        T::Response: IntoResponse,
        T::Future: Send + 'static;
    fn nest(&mut self, path: &str, router2: Router<()>) -> &mut Self;
    fn nest_service<T>(&mut self, path: &str, service: T) -> &mut Self
    where
        T: Service<axum::extract::Request, Error = Infallible> + Clone + Send + Sync + 'static,
        T::Response: IntoResponse,
        T::Future: Send + 'static;
    fn merge<R>(&mut self, other: R) -> &mut Self
    where
        R: Into<Router<()>>;
    fn layer<L>(&mut self, layer: L) -> &mut Self
    where
        L: Layer<Route> + Clone + Send + Sync + 'static,
        L::Service: Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as Service<axum::extract::Request>>::Response: IntoResponse + 'static,
        <L::Service as Service<axum::extract::Request>>::Error: Into<Infallible> + 'static,
        <L::Service as Service<axum::extract::Request>>::Future: Send + 'static;
    fn route_layer<L>(&mut self, layer: L) -> &mut Self
    where
        L: Layer<Route> + Clone + Send + Sync + 'static,
        L::Service: Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as Service<axum::extract::Request>>::Response: IntoResponse + 'static,
        <L::Service as Service<axum::extract::Request>>::Error: Into<Infallible> + 'static,
        <L::Service as Service<axum::extract::Request>>::Future: Send + 'static;
    fn fallback<H, T>(&mut self, handler: H) -> &mut Self
    where
        H: Handler<T, ()>,
        T: 'static;
    fn fallback_service<T>(&mut self, service: T) -> &mut Self
    where
        T: Service<axum::extract::Request, Error = Infallible> + Clone + Send + Sync + 'static,
        T::Response: IntoResponse,
        T::Future: Send + 'static;
    fn method_not_allowed_fallback<H, T>(&mut self, handler: H) -> &mut Self
    where
        H: Handler<T, ()>,
        T: 'static;
}

impl RouterAppExt for App {
    fn router(&mut self, router_fn: impl FnOnce(Router) -> Router) {
        self.world_mut().init_resource::<WebServerManager>();
        if !self.is_plugin_added::<BevyWebServerPlugin>() {
            self.add_plugins(BevyWebServerPlugin);
        }

        self.world_mut()
            .resource_scope(|world, mut manager: Mut<WebServerManager>| {
                let (default_ip, default_port) = world
                    .get_resource::<WebServerConfig>()
                    .map_or((DEFAULT_IP, DEFAULT_PORT), |config| {
                        (config.ip, config.port)
                    });
                if !manager.has_server(&default_port) {
                    let _ =
                        manager.add_server(WebServer::new(default_ip, default_port, Router::new()));
                }

                let existing_router = manager
                    .get_server(&default_port)
                    .map(|srv| srv.router().clone())
                    .unwrap_or_default();

                let new_router = router_fn(existing_router);
                manager.set_router(&default_port, new_router);
            });
    }

    fn route(&mut self, path: &str, method_router: MethodRouter<()>) -> &mut Self {
        self.router(|router| router.route(path, method_router));
        self
    }

    fn route_service<T>(&mut self, path: &str, service: T) -> &mut Self
    where
        T: Service<axum::extract::Request, Error = Infallible> + Clone + Send + Sync + 'static,
        T::Response: IntoResponse,
        T::Future: Send + 'static,
    {
        self.router(|r| r.route_service(path, service));
        self
    }

    fn nest(&mut self, path: &str, router2: Router<()>) -> &mut Self {
        self.router(|r| r.nest(path, router2));
        self
    }

    fn nest_service<T>(&mut self, path: &str, service: T) -> &mut Self
    where
        T: Service<axum::extract::Request, Error = Infallible> + Clone + Send + Sync + 'static,
        T::Response: IntoResponse,
        T::Future: Send + 'static,
    {
        self.router(|r| r.nest_service(path, service));
        self
    }

    fn merge<R>(&mut self, other: R) -> &mut Self
    where
        R: Into<Router<()>>,
    {
        self.router(|r| r.merge(other));
        self
    }

    fn layer<L>(&mut self, layer: L) -> &mut Self
    where
        L: Layer<Route> + Clone + Send + Sync + 'static,
        L::Service: Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as Service<axum::extract::Request>>::Response: IntoResponse + 'static,
        <L::Service as Service<axum::extract::Request>>::Error: Into<Infallible> + 'static,
        <L::Service as Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.router(|r| r.layer(layer));
        self
    }

    fn route_layer<L>(&mut self, layer: L) -> &mut Self
    where
        L: Layer<Route> + Clone + Send + Sync + 'static,
        L::Service: Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as Service<axum::extract::Request>>::Response: IntoResponse + 'static,
        <L::Service as Service<axum::extract::Request>>::Error: Into<Infallible> + 'static,
        <L::Service as Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.router(|r| r.route_layer(layer));
        self
    }

    fn fallback<H, T>(&mut self, handler: H) -> &mut Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.router(|r| r.fallback(handler));
        self
    }

    fn fallback_service<T>(&mut self, service: T) -> &mut Self
    where
        T: Service<axum::extract::Request, Error = Infallible> + Clone + Send + Sync + 'static,
        T::Response: IntoResponse,
        T::Future: Send + 'static,
    {
        self.router(|r| r.fallback_service(service));
        self
    }

    fn method_not_allowed_fallback<H, T>(&mut self, handler: H) -> &mut Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.router(|r| r.method_not_allowed_fallback(handler));
        self
    }
}
