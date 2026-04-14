use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{Json, extract::State, http::StatusCode};
use rand::rngs::OsRng;

use crate::AppState;
use crate::auth::create_token;
use crate::models::{AuthResponse, LoginRequest, RegisterRequest, User};
use crate::pow;

#[derive(serde::Serialize)]
pub struct ChallengeResponse {
    pub challenge: String,
    pub difficulty: usize,
}

pub async fn challenge() -> Json<ChallengeResponse> {
    Json(ChallengeResponse {
        challenge: pow::generate_challenge(),
        difficulty: 4,
    })
}

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<AuthResponse>, StatusCode> {
    if !pow::verify(&req.challenge, &req.nonce) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .to_string();

    let user: User = sqlx::query_as(
        "INSERT INTO users (username, password_hash) VALUES ($1, $2) RETURNING *",
    )
    .bind(&req.username)
    .bind(&password_hash)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::CONFLICT)?; // username already exists

    let token = create_token(user.id)?;
    Ok(Json(AuthResponse { token }))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, StatusCode> {
    if !pow::verify(&req.challenge, &req.nonce) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let user: User = sqlx::query_as("SELECT * FROM users WHERE username = $1")
        .bind(&req.username)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let parsed_hash =
        PasswordHash::new(&user.password_hash).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed_hash)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    let token = create_token(user.id)?;
    Ok(Json(AuthResponse { token }))
}
