use axum::{
    Router,
    http::HeaderValue,
    routing::{get, post},
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

mod auth;
mod game;
mod handlers;
mod models;
pub mod pow;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub games: game::GameManager,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://kahoot:kahoot@db:5432/kahoot".to_string());

    let max_connections: u32 = std::env::var("DB_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);

    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
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

    let admin = Router::new()
        .route("/users", get(handlers::admin::list_users))
        .route("/users/{id}/quizzes", get(handlers::admin::user_quizzes))
        .route("/users/{id}/password", axum::routing::put(handlers::admin::change_password))
        .route("/users/{id}/admin", axum::routing::put(handlers::admin::set_admin))
        .route("/users/{id}", axum::routing::delete(handlers::admin::delete_user))
        .route("/quizzes/{id}", axum::routing::delete(handlers::admin::delete_quiz));

    let api = Router::new()
        .nest("/admin", admin)
        .route("/auth/challenge", get(handlers::auth::challenge))
        .route("/auth/register", post(handlers::auth::register))
        .route("/auth/login", post(handlers::auth::login))
        .route("/auth/me", get(handlers::auth::me))
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
        .fallback_service(
            ServeDir::new("static")
                .precompressed_gzip()
        )
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=3600"),
        ))
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
