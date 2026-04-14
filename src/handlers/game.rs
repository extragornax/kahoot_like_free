use axum::{
    Json,
    extract::{Path, Query, State, ws::{Message, WebSocket, WebSocketUpgrade}},
    http::StatusCode,
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::AppState;
use crate::auth::AuthUser;
use crate::game::{self, AnswerChoice, GamePhase, GameSession, Player, PlayerAnswer, QuestionData, QuizData};
use crate::models::{Answer, Question};

// --- REST: create a game session from a quiz ---

#[derive(serde::Serialize)]
pub struct CreateGameResponse {
    pub pin: String,
}

pub async fn create(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(quiz_id): Path<Uuid>,
) -> Result<Json<CreateGameResponse>, StatusCode> {
    // Load quiz + questions + answers from DB
    let quiz = sqlx::query_as::<_, crate::models::Quiz>(
        "SELECT * FROM quizzes WHERE id = $1 AND creator_id = $2",
    )
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

    let mut quiz_questions = Vec::new();
    for q in &questions {
        let answers: Vec<Answer> =
            sqlx::query_as("SELECT * FROM answers WHERE question_id = $1 ORDER BY position")
                .bind(q.id)
                .fetch_all(&state.db)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        quiz_questions.push(QuestionData {
            text: q.text.clone(),
            answers: answers
                .into_iter()
                .map(|a| AnswerChoice {
                    text: a.text,
                    is_correct: a.is_correct,
                })
                .collect(),
            time_limit_secs: q.time_limit_secs,
            image_url: q.image_url.clone(),
        });
    }

    if quiz_questions.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let quiz_data = QuizData {
        title: quiz.title,
        questions: quiz_questions,
        background_url: quiz.background_url,
        music_url: quiz.music_url,
    };

    let mut games = state.games.write().await;
    let pin = loop {
        let candidate = game::generate_pin();
        if !games.contains_key(&candidate) {
            break candidate;
        }
    };

    games.insert(pin.clone(), GameSession::new(pin.clone(), quiz_data));

    Ok(Json(CreateGameResponse { pin }))
}

// --- QR code SVG ---

#[derive(serde::Deserialize)]
pub struct QrQuery {
    pub url: String,
}

pub async fn qr_svg(
    Path(pin): Path<String>,
    Query(query): Query<QrQuery>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    // Verify the game exists
    let games = state.games.read().await;
    if !games.contains_key(&pin) {
        return Err(StatusCode::NOT_FOUND);
    }
    drop(games);

    let code = qrcode::QrCode::new(query.url.as_bytes()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#ffffff"))
        .light_color(qrcode::render::svg::Color("#46178f"))
        .build();

    Ok(([(axum::http::header::CONTENT_TYPE, "image/svg+xml")], svg))
}

// --- WebSocket: host ---

pub async fn host_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(pin): Path<String>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_host(socket, state, pin))
}

async fn handle_host(socket: WebSocket, state: AppState, pin: String) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Register host
    {
        let mut games = state.games.write().await;
        let Some(session) = games.get_mut(&pin) else {
            let _ = ws_sender
                .send(Message::Text(
                    json!({"type": "error", "message": "Game not found"})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        };
        session.host_tx = Some(tx);

        let player_list: Vec<_> = session.players.values().map(|p| p.nickname.clone()).collect();
        let _ = ws_sender
            .send(Message::Text(
                json!({
                    "type": "lobby",
                    "pin": pin,
                    "quiz_title": session.quiz.title,
                    "players": player_list,
                    "background_url": session.quiz.background_url,
                    "music_url": session.quiz.music_url,
                })
                .to_string()
                .into(),
            ))
            .await;
    }

    // Forward channel → WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Read messages from host
    while let Some(Ok(msg)) = ws_receiver.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(msg_type) = parsed["type"].as_str() else {
            continue;
        };

        match msg_type {
            "start" => {
                let mut games = state.games.write().await;
                if let Some(session) = games.get_mut(&pin) {
                    if session.phase == GamePhase::Lobby {
                        start_question(session, &state, &pin);
                    }
                }
            }
            "next" => {
                let mut games = state.games.write().await;
                if let Some(session) = games.get_mut(&pin) {
                    if session.phase == GamePhase::Results {
                        if session.current_question + 1 < session.quiz.questions.len() {
                            session.current_question += 1;
                            start_question(session, &state, &pin);
                        } else {
                            finish_game(session);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Host disconnected — tear down game
    send_task.abort();
    let mut games = state.games.write().await;
    if let Some(session) = games.get_mut(&pin) {
        session.send_to_all_players(
            &json!({"type": "game_over", "reason": "Host disconnected"}).to_string(),
        );
    }
    games.remove(&pin);
}

// --- WebSocket: player ---

pub async fn player_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(pin): Path<String>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_player(socket, state, pin))
}

async fn handle_player(socket: WebSocket, state: AppState, pin: String) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let player_id = Uuid::new_v4().to_string();

    // Wait for join message with nickname
    let nickname = loop {
        match ws_receiver.next().await {
            Some(Ok(Message::Text(text))) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                    if parsed["type"].as_str() == Some("join") {
                        if let Some(nick) = parsed["nickname"].as_str() {
                            if !nick.trim().is_empty() {
                                break nick.trim().to_string();
                            }
                        }
                    }
                }
            }
            _ => return,
        }
    };

    // Register player in game session
    {
        let mut games = state.games.write().await;
        let Some(session) = games.get_mut(&pin) else {
            let _ = ws_sender
                .send(Message::Text(
                    json!({"type": "error", "message": "Game not found"})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        };

        if session.phase != GamePhase::Lobby {
            let _ = ws_sender
                .send(Message::Text(
                    json!({"type": "error", "message": "Game already started"})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }

        session.players.insert(
            player_id.clone(),
            Player {
                nickname: nickname.clone(),
                score: 0,
                tx,
            },
        );

        let player_count = session.players.len();
        session.send_to_host(
            &json!({
                "type": "player_joined",
                "nickname": nickname,
                "player_count": player_count,
            })
            .to_string(),
        );

        let _ = ws_sender
            .send(Message::Text(
                json!({
                    "type": "joined",
                    "message": "Waiting for host to start the game...",
                    "background_url": session.quiz.background_url,
                })
                .to_string()
                .into(),
            ))
            .await;
    }

    // Forward channel → WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Read messages from player
    while let Some(Ok(msg)) = ws_receiver.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(msg_type) = parsed["type"].as_str() else {
            continue;
        };

        if msg_type == "answer" {
            if let Some(index) = parsed["index"].as_u64() {
                let mut games = state.games.write().await;
                if let Some(session) = games.get_mut(&pin) {
                    if session.phase == GamePhase::Question
                        && !session.answers.contains_key(&player_id)
                    {
                        let time_ms = session
                            .question_started_at
                            .map(|t| t.elapsed().as_millis() as u64)
                            .unwrap_or(0);

                        session.answers.insert(
                            player_id.clone(),
                            PlayerAnswer {
                                answer_index: index as usize,
                                time_ms,
                            },
                        );

                        let count = session.answers.len();
                        let total = session.players.len();
                        session.send_to_host(
                            &json!({
                                "type": "answer_count",
                                "count": count,
                                "total": total,
                            })
                            .to_string(),
                        );

                        if let Some(player) = session.players.get(&player_id) {
                            let _ = player.tx.send(
                                json!({"type": "answer_accepted"}).to_string(),
                            );
                        }

                        if session.all_answered() {
                            close_question(session);
                        }
                    }
                }
            }
        }
    }

    // Player disconnected
    send_task.abort();
    let mut games = state.games.write().await;
    if let Some(session) = games.get_mut(&pin) {
        session.players.remove(&player_id);
        let player_count = session.players.len();
        session.send_to_host(
            &json!({
                "type": "player_left",
                "nickname": nickname,
                "player_count": player_count,
            })
            .to_string(),
        );

        if session.phase == GamePhase::Question
            && !session.players.is_empty()
            && session.all_answered()
        {
            close_question(session);
        }
    }
}

// --- Game flow helpers ---

fn start_question(session: &mut GameSession, state: &AppState, pin: &str) {
    session.phase = GamePhase::Question;
    session.answers.clear();
    session.question_started_at = Some(std::time::Instant::now());

    let q = &session.quiz.questions[session.current_question];
    let idx = session.current_question;
    let total = session.quiz.questions.len();

    // Host sees correct answers
    session.send_to_host(
        &json!({
            "type": "question",
            "index": idx,
            "total": total,
            "text": q.text,
            "image_url": q.image_url,
            "answers": q.answers.iter().map(|a| json!({
                "text": a.text,
                "is_correct": a.is_correct,
            })).collect::<Vec<_>>(),
            "time_limit": q.time_limit_secs,
        })
        .to_string(),
    );

    // Players see answer texts only (no correct flag)
    session.send_to_all_players(
        &json!({
            "type": "question",
            "index": idx,
            "total": total,
            "text": q.text,
            "image_url": q.image_url,
            "answers": q.answers.iter().map(|a| a.text.clone()).collect::<Vec<_>>(),
            "time_limit": q.time_limit_secs,
        })
        .to_string(),
    );

    // Auto-close after time limit
    let games = state.games.clone();
    let pin = pin.to_string();
    let question_idx = session.current_question;
    let time_limit = q.time_limit_secs;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(time_limit as u64)).await;
        let mut games = games.write().await;
        if let Some(session) = games.get_mut(&pin) {
            if session.phase == GamePhase::Question && session.current_question == question_idx {
                close_question(session);
            }
        }
    });
}

fn close_question(session: &mut GameSession) {
    // Snapshot scores before this round for delta calculation
    let prev_scores: std::collections::HashMap<String, i64> = session
        .players
        .iter()
        .map(|(id, p)| (id.clone(), p.score))
        .collect();

    session.phase = GamePhase::Results;
    let q = &session.quiz.questions[session.current_question];
    let time_limit_ms = q.time_limit_secs as u64 * 1000;

    // Score each answer, track timing
    let mut player_results =
        std::collections::HashMap::<String, (bool, i64, u64)>::new(); // (correct, points, time_ms)
    let mut answer_times: Vec<u64> = Vec::new();

    for (player_id, answer) in &session.answers {
        let correct = q
            .answers
            .get(answer.answer_index)
            .map(|a| a.is_correct)
            .unwrap_or(false);

        let points = if correct && time_limit_ms > 0 {
            let time_taken = answer.time_ms.min(time_limit_ms);
            (1000 - (500 * time_taken / time_limit_ms)) as i64
        } else {
            0
        };

        if let Some(player) = session.players.get_mut(player_id) {
            player.score += points;
        }
        answer_times.push(answer.time_ms);
        player_results.insert(player_id.clone(), (correct, points, answer.time_ms));
    }

    // Speed stats
    answer_times.sort();
    let fastest_ms = answer_times.first().copied().unwrap_or(0);
    let average_ms = if answer_times.is_empty() {
        0
    } else {
        answer_times.iter().sum::<u64>() / answer_times.len() as u64
    };

    let leaderboard = session.leaderboard();

    // Answer distribution
    let mut answer_counts = vec![0usize; q.answers.len()];
    for answer in session.answers.values() {
        if answer.answer_index < answer_counts.len() {
            answer_counts[answer.answer_index] += 1;
        }
    }

    let is_last = session.current_question + 1 >= session.quiz.questions.len();

    // Host results — include score gained per player for animations
    session.send_to_host(
        &json!({
            "type": "results",
            "answers": q.answers.iter().enumerate().map(|(i, a)| json!({
                "text": a.text,
                "is_correct": a.is_correct,
                "count": answer_counts[i],
            })).collect::<Vec<_>>(),
            "leaderboard": leaderboard.iter().map(|e| {
                let prev = prev_scores.values()
                    .zip(session.players.values())
                    .find(|(_, p)| p.nickname == e.nickname)
                    .map(|(_, p)| prev_scores.iter().find(|(id, _)| {
                        session.players.get(*id).map(|pl| pl.nickname == e.nickname).unwrap_or(false)
                    }).map(|(_, s)| *s).unwrap_or(0))
                    .unwrap_or(0);
                json!({
                    "nickname": e.nickname,
                    "score": e.score,
                    "gained": e.score - prev,
                })
            }).collect::<Vec<_>>(),
            "is_last": is_last,
            "fastest_ms": fastest_ms,
        })
        .to_string(),
    );

    // Individual results to each player — include timing stats
    for (player_id, player) in &session.players {
        let (correct, points, time_ms) =
            player_results.get(player_id).copied().unwrap_or((false, 0, 0));
        let rank = leaderboard
            .iter()
            .position(|e| e.nickname == player.nickname)
            .unwrap_or(0)
            + 1;
        // Speed rank: how many answered faster
        let speed_rank = answer_times.iter().filter(|&&t| t < time_ms).count() + 1;
        let answered = player_results.contains_key(player_id);

        let _ = player.tx.send(
            json!({
                "type": "result",
                "correct": correct,
                "points": points,
                "score": player.score,
                "rank": rank,
                "total_players": session.players.len(),
                "time_ms": if answered { time_ms } else { 0 },
                "speed_rank": if answered { speed_rank } else { 0 },
                "fastest_ms": fastest_ms,
                "average_ms": average_ms,
                "total_answered": answer_times.len(),
            })
            .to_string(),
        );
    }
}

fn finish_game(session: &mut GameSession) {
    session.phase = GamePhase::Finished;
    let leaderboard = session.leaderboard();

    session.send_to_host(&json!({"type": "finished", "leaderboard": leaderboard}).to_string());

    for (_, player) in &session.players {
        let rank = leaderboard
            .iter()
            .position(|e| e.nickname == player.nickname)
            .unwrap_or(0)
            + 1;
        let _ = player.tx.send(
            json!({
                "type": "finished",
                "rank": rank,
                "score": player.score,
                "leaderboard": leaderboard,
            })
            .to_string(),
        );
    }
}
