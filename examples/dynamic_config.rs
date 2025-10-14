use axum::{response::Html, routing::get, Router};
use bevy::prelude::*;
use bevy_defer::AsyncExecutor;
use bevy_webgate::prelude::*;
use std::net::{IpAddr, Ipv4Addr};

use std::time::Duration;

#[derive(Resource)]
struct ConfigTimer(Timer);

#[derive(Default, Resource)]
struct ServerState {
    enabled: bool,
}

fn configure_routes(port: WebPort) -> Router {
    Router::new().route(
        "/",
        get(move || async move {
            Html(format!(
                "<h1>Server on Port {}</h1><p>Visit <a href='http://localhost:{}'>Port {}</a></p>",
                port, port, port
            ))
        }),
    )
}

fn main() {
    let mut app = App::new();

    app.add_plugins(DefaultPlugins)
        .add_plugins(bevy_webgate::BevyWebServerPlugin);

    app.insert_resource({
        let mut timer = Timer::new(Duration::from_secs(30), TimerMode::Repeating);

        // Start with timer already finished to trigger the first run immediately
        timer.set_elapsed(Duration::from_secs(30));
        ConfigTimer(timer)
    })
    .init_resource::<ServerState>();

    // Default server on port 8080
    app.route(
        "/",
        get(|| async { Html("<h1>Welcome to the Dynamic Config Example</h1>") }),
    );

    app.add_systems(Update, dynamic_reconfigure_servers);

    app.run();
}

fn dynamic_reconfigure_servers(
    time: Res<Time>,
    mut timer: ResMut<ConfigTimer>,
    mut manager: ResMut<WebServerManager>,
    mut server_state: ResMut<ServerState>,
    async_executor: NonSend<AsyncExecutor>,
) {
    if timer.0.tick(time.delta()).is_finished() {
        if server_state.enabled {
            manager.stop_server(&8080);
            manager.remove_server(&8081);
            manager.remove_server(&8082);
        } else {
            let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
            let _ = manager.start_server(&8080, &async_executor);
            let _ = manager.add_server(WebServer::new(ip, 8081, configure_routes(8081)));
            let _ = manager.add_server(WebServer::new(ip, 8082, configure_routes(8082)));
        }

        server_state.enabled = !server_state.enabled;
        info!(
            "Server state toggled: now enabled = {}",
            server_state.enabled
        );
    }
}
