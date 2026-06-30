use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{Json, extract::State, http::StatusCode};
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::auth::create_token;
use crate::models::{
    AuthResponse, ForgotPasswordRequest, LoginRequest, RegisterRequest, ResetPasswordRequest,
    UpdateAccountRequest, User,
};
use crate::pow;

const RESET_TOKEN_TTL_MINUTES: i64 = 60;

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

    let email = req.email.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let user: User = sqlx::query_as(
        "INSERT INTO users (username, password_hash, email) VALUES ($1, $2, $3) RETURNING *",
    )
    .bind(&req.username)
    .bind(&password_hash)
    .bind(email)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::CONFLICT)?; // username or email already exists

    let token = create_token(user.id, user.is_admin)?;
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

    let token = create_token(user.id, user.is_admin)?;
    Ok(Json(AuthResponse { token }))
}

#[derive(serde::Serialize)]
pub struct MeResponse {
    pub id: uuid::Uuid,
    pub username: String,
    pub email: Option<String>,
    pub is_admin: bool,
}

pub async fn me(
    crate::auth::AuthUser(user_id, _): crate::auth::AuthUser,
    State(state): State<AppState>,
) -> Result<Json<MeResponse>, StatusCode> {
    let user: User = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(MeResponse {
        id: user.id,
        username: user.username,
        email: user.email,
        is_admin: user.is_admin,
    }))
}

/// Update the authenticated user's email and/or password. Requires the current
/// password to authorize the change.
pub async fn update_account(
    crate::auth::AuthUser(user_id, _): crate::auth::AuthUser,
    State(state): State<AppState>,
    Json(req): Json<UpdateAccountRequest>,
) -> Result<StatusCode, StatusCode> {
    let user: User = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let parsed_hash =
        PasswordHash::new(&user.password_hash).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Argon2::default()
        .verify_password(req.current_password.as_bytes(), &parsed_hash)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    if let Some(email) = &req.email {
        let email = email.trim();
        let email = (!email.is_empty()).then_some(email);
        sqlx::query("UPDATE users SET email = $1 WHERE id = $2")
            .bind(email)
            .bind(user_id)
            .execute(&state.db)
            .await
            .map_err(|_| StatusCode::CONFLICT)?; // email already in use
    }

    if let Some(new_password) = &req.new_password {
        if !new_password.is_empty() {
            let salt = SaltString::generate(&mut OsRng);
            let password_hash = Argon2::default()
                .hash_password(new_password.as_bytes(), &salt)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .to_string();
            sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
                .bind(&password_hash)
                .bind(user_id)
                .execute(&state.db)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

// --- Password recovery ---

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Request a password reset link. Always returns 200 with the same body so the
/// endpoint cannot be used to discover which emails have accounts.
pub async fn forgot_password(
    State(state): State<AppState>,
    Json(req): Json<ForgotPasswordRequest>,
) -> Result<StatusCode, StatusCode> {
    if !pow::verify(&req.challenge, &req.nonce) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let email = req.email.trim();

    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Only generate a token when the email is known; either way we return 200.
    if let Some(user) = user {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token = hex::encode(bytes);
        let expires_at = chrono::Utc::now() + chrono::Duration::minutes(RESET_TOKEN_TTL_MINUTES);

        sqlx::query(
            "INSERT INTO password_reset_tokens (user_id, token_hash, expires_at) VALUES ($1, $2, $3)",
        )
        .bind(user.id)
        .bind(hash_token(&token))
        .bind(expires_at)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        send_reset_email(email, &token);
    }

    Ok(StatusCode::OK)
}

/// Deliver the reset link. No email provider is configured, so in dev we log it.
// TODO: wire an SMTP/email provider for production delivery.
fn send_reset_email(email: &str, token: &str) {
    let link = match std::env::var("APP_BASE_URL") {
        Ok(base) => format!("{}/?reset={}", base.trim_end_matches('/'), token),
        Err(_) => format!("/?reset={token}"),
    };
    tracing::info!("password reset requested for {email}: {link}");
}

/// Complete a password reset using a token from the reset link.
pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> Result<StatusCode, StatusCode> {
    let token_hash = hash_token(req.token.trim());

    // Atomically consume the token: only succeeds if unused and unexpired.
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "UPDATE password_reset_tokens SET used_at = now() \
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now() \
         RETURNING user_id",
    )
    .bind(&token_hash)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some((user_id,)) = row else {
        return Err(StatusCode::BAD_REQUEST); // invalid, used, or expired token
    };

    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .to_string();

    sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
        .bind(&password_hash)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}
