use axum::{Json, extract::State, http::StatusCode};

use crate::AppState;
use crate::auth::AuthUser;
use crate::handlers::upload::delete_upload;
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
        "INSERT INTO quizzes (title, creator_id, background_url, music_url) VALUES ($1, $2, $3, $4) RETURNING *",
    )
    .bind(&req.title)
    .bind(user_id)
    .bind(&req.background_url)
    .bind(&req.music_url)
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

pub async fn update(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    axum::extract::Path(quiz_id): axum::extract::Path<uuid::Uuid>,
    Json(req): Json<CreateQuizRequest>,
) -> Result<Json<Quiz>, StatusCode> {
    // Fetch old quiz to clean up replaced media
    let old_quiz: Quiz =
        sqlx::query_as("SELECT * FROM quizzes WHERE id = $1 AND creator_id = $2")
            .bind(quiz_id)
            .bind(user_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?;

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let quiz: Quiz = sqlx::query_as(
        "UPDATE quizzes SET title = $1, background_url = $4, music_url = $5, updated_at = now() WHERE id = $2 AND creator_id = $3 RETURNING *",
    )
    .bind(&req.title)
    .bind(quiz_id)
    .bind(user_id)
    .bind(&req.background_url)
    .bind(&req.music_url)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    // Delete old questions (CASCADE deletes answers)
    sqlx::query("DELETE FROM questions WHERE quiz_id = $1")
        .bind(quiz_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Re-insert questions and answers
    for (pos, q) in req.questions.iter().enumerate() {
        let question: Question = sqlx::query_as(
            "INSERT INTO questions (quiz_id, text, time_limit_secs, position) VALUES ($1, $2, $3, $4) RETURNING *",
        )
        .bind(quiz_id)
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

    // Clean up replaced media files
    if old_quiz.background_url != req.background_url {
        if let Some(url) = &old_quiz.background_url {
            delete_upload(url);
        }
    }
    if old_quiz.music_url != req.music_url {
        if let Some(url) = &old_quiz.music_url {
            delete_upload(url);
        }
    }

    Ok(Json(quiz))
}

pub async fn delete(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    axum::extract::Path(quiz_id): axum::extract::Path<uuid::Uuid>,
) -> Result<StatusCode, StatusCode> {
    // Fetch quiz first to clean up media
    let quiz: Option<Quiz> =
        sqlx::query_as("SELECT * FROM quizzes WHERE id = $1 AND creator_id = $2")
            .bind(quiz_id)
            .bind(user_id)
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

    // Clean up media files
    if let Some(url) = &quiz.background_url {
        delete_upload(url);
    }
    if let Some(url) = &quiz.music_url {
        delete_upload(url);
    }

    Ok(StatusCode::NO_CONTENT)
}
