use axum::{
    Router,
    routing::{delete, get, post, put},
};
use sqlx::PgPool;

pub mod auth;
pub mod game;
pub mod handlers;
pub mod models;
pub mod pow;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub games: game::GameManager,
}

pub fn build_app(state: AppState) -> Router {
    let admin = Router::new()
        .route("/users", get(handlers::admin::list_users))
        .route("/users/{id}/quizzes", get(handlers::admin::user_quizzes))
        .route("/users/{id}/password", put(handlers::admin::change_password))
        .route("/users/{id}/admin", put(handlers::admin::set_admin))
        .route("/users/{id}", delete(handlers::admin::delete_user))
        .route("/quizzes/{id}", delete(handlers::admin::delete_quiz));

    let api = Router::new()
        .nest("/admin", admin)
        .route("/auth/challenge", get(handlers::auth::challenge))
        .route("/auth/register", post(handlers::auth::register))
        .route("/auth/login", post(handlers::auth::login))
        .route("/auth/me", get(handlers::auth::me))
        .route(
            "/quizzes",
            get(handlers::quiz::list).post(handlers::quiz::create),
        )
        .route(
            "/quizzes/{id}",
            get(handlers::quiz::get)
                .put(handlers::quiz::update)
                .delete(handlers::quiz::delete),
        )
        .route("/games/{quiz_id}", post(handlers::game::create))
        .route("/games/{pin}/qr", get(handlers::game::qr_svg))
        .route(
            "/upload",
            post(handlers::upload::upload)
                .layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024)),
        );

    Router::new()
        .nest("/api", api)
        .route("/ws/host/{pin}", get(handlers::game::host_ws))
        .route("/ws/play/{pin}", get(handlers::game::player_ws))
        .route("/health", get(|| async { "ok" }))
        .with_state(state)
}
