use axum::{Json, extract::Path, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::AppState;
use crate::auth::AuthUser;
use crate::game::GameSession;

// --- Persistence: snapshot a finished session and write it to the DB ---

pub struct HistoryRecord {
    pub host_id: Uuid,
    pub quiz_id: Uuid,
    pub quiz_title: String,
    pub players: Vec<PlayerRecord>,
    pub questions: Vec<QuestionRecord>,
}

pub struct PlayerRecord {
    pub nickname: String,
    pub avatar: String,
    pub score: i64,
    pub correct_count: i32,
    pub best_streak: i32,
}

pub struct QuestionRecord {
    pub text: String,
    pub kind: String,
    pub answered_count: i32,
    pub correct_count: Option<i32>,
    pub player_count: i32,
}

impl HistoryRecord {
    /// Snapshot everything the history tables need. Called under the session
    /// lock, so it only clones — the DB write happens later, lock-free.
    pub fn from_session(session: &GameSession) -> Self {
        let mut players: Vec<PlayerRecord> = session
            .players
            .values()
            .map(|p| PlayerRecord {
                nickname: p.nickname.clone(),
                avatar: p.avatar.clone(),
                score: p.score,
                correct_count: p.correct_count as i32,
                best_streak: p.best_streak as i32,
            })
            // Players who dropped mid-game (locked phone, dead battery) still
            // played — record them alongside the connected ones.
            .chain(
                session
                    .disconnected
                    .iter()
                    .map(|(nickname, d)| PlayerRecord {
                        nickname: nickname.clone(),
                        avatar: d.avatar.clone(),
                        score: d.score,
                        correct_count: d.correct_count as i32,
                        best_streak: d.best_streak as i32,
                    }),
            )
            .collect();
        players.sort_by(|a, b| b.score.cmp(&a.score));

        let questions = session
            .question_stats
            .iter()
            .map(|s| QuestionRecord {
                text: s.text.clone(),
                kind: s.kind.clone(),
                answered_count: s.answered as i32,
                correct_count: s.correct.map(|c| c as i32),
                player_count: s.player_count as i32,
            })
            .collect();

        Self {
            host_id: session.host_id,
            quiz_id: session.quiz_id,
            quiz_title: session.quiz.title.clone(),
            players,
            questions,
        }
    }
}

pub async fn persist(db: &PgPool, record: HistoryRecord) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;

    // The quiz_id subquery resolves to NULL if the quiz was deleted while the
    // game was running, instead of failing the foreign key.
    let (game_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO game_history (host_id, quiz_id, quiz_title, question_count, player_count)
         VALUES ($1, (SELECT id FROM quizzes WHERE id = $2), $3, $4, $5) RETURNING id",
    )
    .bind(record.host_id)
    .bind(record.quiz_id)
    .bind(&record.quiz_title)
    .bind(record.questions.len() as i32)
    .bind(record.players.len() as i32)
    .fetch_one(&mut *tx)
    .await?;

    for (i, p) in record.players.iter().enumerate() {
        sqlx::query(
            "INSERT INTO game_history_players (game_id, rank, nickname, avatar, score, correct_count, best_streak)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(game_id)
        .bind(i as i32 + 1)
        .bind(&p.nickname)
        .bind(&p.avatar)
        .bind(p.score)
        .bind(p.correct_count)
        .bind(p.best_streak)
        .execute(&mut *tx)
        .await?;
    }

    for (i, q) in record.questions.iter().enumerate() {
        sqlx::query(
            "INSERT INTO game_history_questions (game_id, position, text, kind, answered_count, correct_count, player_count)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(game_id)
        .bind(i as i32 + 1)
        .bind(&q.text)
        .bind(&q.kind)
        .bind(q.answered_count)
        .bind(q.correct_count)
        .bind(q.player_count)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await
}

// --- REST: browse past games ---

#[derive(sqlx::FromRow, serde::Serialize)]
pub struct HistoryListEntry {
    pub id: Uuid,
    pub quiz_title: String,
    pub question_count: i32,
    pub player_count: i32,
    pub finished_at: DateTime<Utc>,
    pub winner_nickname: Option<String>,
    pub winner_avatar: Option<String>,
    pub winner_score: Option<i64>,
}

#[derive(sqlx::FromRow, serde::Serialize)]
pub struct HistoryGame {
    pub id: Uuid,
    pub quiz_id: Option<Uuid>,
    pub quiz_title: String,
    pub question_count: i32,
    pub player_count: i32,
    pub finished_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow, serde::Serialize)]
pub struct HistoryPlayer {
    pub rank: i32,
    pub nickname: String,
    pub avatar: String,
    pub score: i64,
    pub correct_count: i32,
    pub best_streak: i32,
}

#[derive(sqlx::FromRow, serde::Serialize)]
pub struct HistoryQuestion {
    pub position: i32,
    pub text: String,
    pub kind: String,
    pub answered_count: i32,
    pub correct_count: Option<i32>,
    pub player_count: i32,
}

#[derive(serde::Serialize)]
pub struct HistoryDetail {
    #[serde(flatten)]
    pub game: HistoryGame,
    pub players: Vec<HistoryPlayer>,
    pub questions: Vec<HistoryQuestion>,
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user_id, _): AuthUser,
) -> Result<Json<Vec<HistoryListEntry>>, StatusCode> {
    let games: Vec<HistoryListEntry> = sqlx::query_as(
        "SELECT h.id, h.quiz_title, h.question_count, h.player_count, h.finished_at,
                w.nickname AS winner_nickname, w.avatar AS winner_avatar, w.score AS winner_score
         FROM game_history h
         LEFT JOIN LATERAL (
             SELECT nickname, avatar, score FROM game_history_players
             WHERE game_id = h.id ORDER BY rank ASC LIMIT 1
         ) w ON true
         WHERE h.host_id = $1
         ORDER BY h.finished_at DESC
         LIMIT 200",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(games))
}

pub async fn get(
    State(state): State<AppState>,
    AuthUser(user_id, _): AuthUser,
    Path(game_id): Path<Uuid>,
) -> Result<Json<HistoryDetail>, StatusCode> {
    let game: HistoryGame = sqlx::query_as(
        "SELECT id, quiz_id, quiz_title, question_count, player_count, finished_at
         FROM game_history WHERE id = $1 AND host_id = $2",
    )
    .bind(game_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let players: Vec<HistoryPlayer> = sqlx::query_as(
        "SELECT rank, nickname, avatar, score, correct_count, best_streak
         FROM game_history_players WHERE game_id = $1 ORDER BY rank ASC",
    )
    .bind(game_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let questions: Vec<HistoryQuestion> = sqlx::query_as(
        "SELECT position, text, kind, answered_count, correct_count, player_count
         FROM game_history_questions WHERE game_id = $1 ORDER BY position ASC",
    )
    .bind(game_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(HistoryDetail {
        game,
        players,
        questions,
    }))
}

pub async fn delete(
    State(state): State<AppState>,
    AuthUser(user_id, _): AuthUser,
    Path(game_id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let result = sqlx::query("DELETE FROM game_history WHERE id = $1 AND host_id = $2")
        .bind(game_id)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(StatusCode::NO_CONTENT)
}
