use axum::{
    Router,
    routing::{get, post},
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower_http::services::ServeDir;

mod auth;
mod handlers;
mod models;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
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

    let state = AppState { db: pool };

    let api = Router::new()
        .route("/auth/register", post(handlers::auth::register))
        .route("/auth/login", post(handlers::auth::login))
        .route("/quizzes", get(handlers::quiz::list).post(handlers::quiz::create))
        .route("/quizzes/{id}", get(handlers::quiz::get))
        .route("/quizzes/{id}", axum::routing::delete(handlers::quiz::delete));

    let app = Router::new()
        .nest("/api", api)
        .route("/health", get(|| async { "ok" }))
        .fallback_service(ServeDir::new("static"))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    tracing::info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}
