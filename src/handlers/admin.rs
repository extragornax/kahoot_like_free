use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{Json, extract::{Path, State}, http::StatusCode};
use rand::rngs::OsRng;
use uuid::Uuid;

use crate::AppState;
use crate::auth::AdminUser;
use crate::handlers::upload::delete_upload;
use crate::models::Quiz;

// --- List users ---

#[derive(serde::Serialize)]
pub struct UserSummary {
    pub id: Uuid,
    pub username: String,
    pub is_admin: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub quiz_count: i64,
}

pub async fn list_users(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
) -> Result<Json<Vec<UserSummary>>, StatusCode> {
    let users = sqlx::query_as::<_, UserRow>(
        "SELECT u.id, u.username, u.is_admin, u.created_at, COUNT(q.id) AS quiz_count \
         FROM users u LEFT JOIN quizzes q ON q.creator_id = u.id \
         GROUP BY u.id ORDER BY u.created_at DESC",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(
        users
            .into_iter()
            .map(|u| UserSummary {
                id: u.id,
                username: u.username,
                is_admin: u.is_admin,
                created_at: u.created_at,
                quiz_count: u.quiz_count.unwrap_or(0),
            })
            .collect(),
    ))
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    username: String,
    is_admin: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    quiz_count: Option<i64>,
}

// --- Get user's quizzes ---

pub async fn user_quizzes(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(user_id): Path<Uuid>,
) -> Result<Json<Vec<Quiz>>, StatusCode> {
    let quizzes: Vec<Quiz> =
        sqlx::query_as("SELECT * FROM quizzes WHERE creator_id = $1 ORDER BY updated_at DESC")
            .bind(user_id)
            .fetch_all(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(quizzes))
}

// --- Change user password ---

#[derive(serde::Deserialize)]
pub struct ChangePasswordRequest {
    pub password: String,
}

pub async fn change_password(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(user_id): Path<Uuid>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<StatusCode, StatusCode> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .to_string();

    let result = sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
        .bind(&hash)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- Toggle admin ---

#[derive(serde::Deserialize)]
pub struct SetAdminRequest {
    pub is_admin: bool,
}

pub async fn set_admin(
    State(state): State<AppState>,
    AdminUser(caller_id): AdminUser,
    Path(user_id): Path<Uuid>,
    Json(req): Json<SetAdminRequest>,
) -> Result<StatusCode, StatusCode> {
    // Prevent removing your own admin
    if caller_id == user_id && !req.is_admin {
        return Err(StatusCode::BAD_REQUEST);
    }

    let result = sqlx::query("UPDATE users SET is_admin = $1 WHERE id = $2")
        .bind(req.is_admin)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- Delete user ---

pub async fn delete_user(
    State(state): State<AppState>,
    AdminUser(caller_id): AdminUser,
    Path(user_id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    if caller_id == user_id {
        return Err(StatusCode::BAD_REQUEST); // Can't delete yourself
    }

    // Clean up media from user's quizzes
    let quizzes: Vec<Quiz> =
        sqlx::query_as("SELECT * FROM quizzes WHERE creator_id = $1")
            .bind(user_id)
            .fetch_all(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    for quiz in &quizzes {
        if let Some(url) = &quiz.background_url {
            delete_upload(url);
        }
        if let Some(url) = &quiz.music_url {
            delete_upload(url);
        }
    }

    // Delete quizzes (CASCADE deletes questions/answers), then user
    sqlx::query("DELETE FROM quizzes WHERE creator_id = $1")
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let result = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- Admin quiz delete (bypass creator check) ---

pub async fn delete_quiz(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(quiz_id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let quiz: Option<Quiz> = sqlx::query_as("SELECT * FROM quizzes WHERE id = $1")
        .bind(quiz_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some(quiz) = quiz else {
        return Err(StatusCode::NOT_FOUND);
    };

    sqlx::query("DELETE FROM quizzes WHERE id = $1")
        .bind(quiz_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if let Some(url) = &quiz.background_url {
        delete_upload(url);
    }
    if let Some(url) = &quiz.music_url {
        delete_upload(url);
    }

    Ok(StatusCode::NO_CONTENT)
}
