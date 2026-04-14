use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Database rows

#[derive(sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow, Serialize)]
pub struct Quiz {
    pub id: Uuid,
    pub title: String,
    pub creator_id: Option<Uuid>,
    pub background_url: Option<String>,
    pub music_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow, Serialize)]
pub struct Question {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub text: String,
    pub time_limit_secs: i32,
    pub position: i32,
}

#[derive(sqlx::FromRow, Serialize)]
pub struct Answer {
    pub id: Uuid,
    pub question_id: Uuid,
    pub text: String,
    pub is_correct: bool,
    pub position: i32,
}

// API request/response types

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub token: String,
}

#[derive(Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub exp: usize,
}

#[derive(Deserialize)]
pub struct CreateQuizRequest {
    pub title: String,
    pub questions: Vec<CreateQuestionRequest>,
    pub background_url: Option<String>,
    pub music_url: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateQuestionRequest {
    pub text: String,
    pub time_limit_secs: Option<i32>,
    pub answers: Vec<CreateAnswerRequest>,
}

#[derive(Deserialize)]
pub struct CreateAnswerRequest {
    pub text: String,
    pub is_correct: bool,
}
