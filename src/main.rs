use axum::{
    Router,
    routing::{get, post},
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower_http::services::ServeDir;

mod auth;
mod game;
mod handlers;
mod models;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub games: game::GameManager,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://kahoot:kahoot@localhost:5433/kahoot".to_string());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to database");

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("failed to run migrations");

    tracing::info!("migrations applied");

    // Ensure uploads directory exists
    tokio::fs::create_dir_all("static/uploads")
        .await
        .expect("failed to create uploads directory");

    let state = AppState {
        db: pool,
        games: game::new_manager(),
    };

    let api = Router::new()
        .route("/auth/register", post(handlers::auth::register))
        .route("/auth/login", post(handlers::auth::login))
        .route("/quizzes", get(handlers::quiz::list).post(handlers::quiz::create))
        .route("/quizzes/{id}", get(handlers::quiz::get).put(handlers::quiz::update).delete(handlers::quiz::delete))
        .route("/games/{quiz_id}", post(handlers::game::create))
        .route("/games/{pin}/qr", get(handlers::game::qr_svg))
        .route("/upload", post(handlers::upload::upload)
            .layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024)));

    let app = Router::new()
        .nest("/api", api)
        .route("/ws/host/{pin}", get(handlers::game::host_ws))
        .route("/ws/play/{pin}", get(handlers::game::player_ws))
        .route("/health", get(|| async { "ok" }))
        .fallback_service(ServeDir::new("static"))
        .with_state(state);

    let port = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3000u16);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await.unwrap();
    tracing::info!("listening on http://localhost:{port}");
    if let Some(ip) = local_network_ip() {
        tracing::info!("network: http://{ip}:{port}");
        tracing::info!("players join at: http://{ip}:{port}/player.html");
    }
    axum::serve(listener, app).await.unwrap();
}

fn local_network_ip() -> Option<std::net::IpAddr> {
    // Connect a UDP socket to a public address to determine the local IP
    // (no actual traffic is sent)
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}
