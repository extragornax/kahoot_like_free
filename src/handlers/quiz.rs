use axum::{Json, extract::State, http::StatusCode};

use crate::AppState;
use crate::auth::AuthUser;
use crate::models::{Answer, CreateQuizRequest, Question, Quiz};

#[derive(serde::Serialize)]
pub struct QuizDetail {
    #[serde(flatten)]
    pub quiz: Quiz,
    pub questions: Vec<QuestionDetail>,
}

#[derive(serde::Serialize)]
pub struct QuestionDetail {
    #[serde(flatten)]
    pub question: Question,
    pub answers: Vec<Answer>,
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<Quiz>>, StatusCode> {
    let quizzes: Vec<Quiz> =
        sqlx::query_as("SELECT * FROM quizzes WHERE creator_id = $1 ORDER BY updated_at DESC")
            .bind(user_id)
            .fetch_all(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(quizzes))
}

pub async fn get(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    axum::extract::Path(quiz_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<QuizDetail>, StatusCode> {
    let quiz: Quiz = sqlx::query_as("SELECT * FROM quizzes WHERE id = $1 AND creator_id = $2")
        .bind(quiz_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let questions: Vec<Question> =
        sqlx::query_as("SELECT * FROM questions WHERE quiz_id = $1 ORDER BY position")
            .bind(quiz_id)
            .fetch_all(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut question_details = Vec::new();
    for question in questions {
        let answers: Vec<Answer> =
            sqlx::query_as("SELECT * FROM answers WHERE question_id = $1 ORDER BY position")
                .bind(question.id)
                .fetch_all(&state.db)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        question_details.push(QuestionDetail { question, answers });
    }

    Ok(Json(QuizDetail {
        quiz,
        questions: question_details,
    }))
}

pub async fn create(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<CreateQuizRequest>,
) -> Result<(StatusCode, Json<Quiz>), StatusCode> {
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let quiz: Quiz = sqlx::query_as(
        "INSERT INTO quizzes (title, creator_id) VALUES ($1, $2) RETURNING *",
    )
    .bind(&req.title)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    for (pos, q) in req.questions.iter().enumerate() {
        let question: Question = sqlx::query_as(
            "INSERT INTO questions (quiz_id, text, time_limit_secs, position) VALUES ($1, $2, $3, $4) RETURNING *",
        )
        .bind(quiz.id)
        .bind(&q.text)
        .bind(q.time_limit_secs.unwrap_or(20))
        .bind(pos as i32)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        for (apos, a) in q.answers.iter().enumerate() {
            sqlx::query(
                "INSERT INTO answers (question_id, text, is_correct, position) VALUES ($1, $2, $3, $4)",
            )
            .bind(question.id)
            .bind(&a.text)
            .bind(a.is_correct)
            .bind(apos as i32)
            .execute(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }

    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((StatusCode::CREATED, Json(quiz)))
}

pub async fn delete(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    axum::extract::Path(quiz_id): axum::extract::Path<uuid::Uuid>,
) -> Result<StatusCode, StatusCode> {
    let result = sqlx::query("DELETE FROM quizzes WHERE id = $1 AND creator_id = $2")
        .bind(quiz_id)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(StatusCode::NO_CONTENT)
}
