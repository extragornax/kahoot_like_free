use axum::{
    Json,
    extract::{Path, Query, State, ws::{Message, WebSocket, WebSocketUpgrade}},
    http::StatusCode,
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::AppState;
use crate::auth::AuthUser;
use crate::game::{self, AnswerChoice, GamePhase, GameSession, Player, PlayerAnswer, QuestionData, QuizData, StreakMode};
use crate::models::{Answer, Question};

/// Seconds players have to vote on open answers.
const VOTE_TIME_SECS: u64 = 30;

/// Emoji reactions accepted from players in the lobby. Keeping this short and
/// curated avoids treating arbitrary client input as renderable content.
const ALLOWED_REACTIONS: &[&str] = &["👍", "❤️", "😂", "😮", "🎉", "🔥"];

fn is_allowed_reaction(emoji: &str) -> bool {
    ALLOWED_REACTIONS.iter().any(|&e| e == emoji)
}

// --- REST: create a game session from a quiz ---

#[derive(serde::Serialize)]
pub struct CreateGameResponse {
    pub pin: String,
}

pub async fn create(
    State(state): State<AppState>,
    AuthUser(user_id, is_admin): AuthUser,
    Path(quiz_id): Path<Uuid>,
) -> Result<Json<CreateGameResponse>, StatusCode> {
    let quiz = if is_admin {
        sqlx::query_as::<_, crate::models::Quiz>("SELECT * FROM quizzes WHERE id = $1")
            .bind(quiz_id)
            .fetch_optional(&state.db)
            .await
    } else {
        sqlx::query_as::<_, crate::models::Quiz>(
            "SELECT * FROM quizzes WHERE id = $1 AND creator_id = $2",
        )
        .bind(quiz_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
    }
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let questions: Vec<Question> =
        sqlx::query_as("SELECT * FROM questions WHERE quiz_id = $1 ORDER BY position")
            .bind(quiz_id)
            .fetch_all(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if questions.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Batch fetch all answers (avoids N+1)
    let question_ids: Vec<Uuid> = questions.iter().map(|q| q.id).collect();
    let all_answers: Vec<Answer> =
        sqlx::query_as("SELECT * FROM answers WHERE question_id = ANY($1) ORDER BY question_id, position")
            .bind(&question_ids)
            .fetch_all(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut answers_by_question: HashMap<Uuid, Vec<Answer>> = HashMap::new();
    for answer in all_answers {
        answers_by_question.entry(answer.question_id).or_default().push(answer);
    }

    let quiz_questions: Vec<QuestionData> = questions
        .iter()
        .map(|q| {
            let answers = answers_by_question.remove(&q.id).unwrap_or_default();
            QuestionData {
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
                kind: q.kind.clone(),
            }
        })
        .collect();

    let quiz_data = QuizData {
        title: quiz.title,
        questions: quiz_questions,
        background_url: quiz.background_url,
        music_url: quiz.music_url,
    };

    let pin = loop {
        let candidate = game::generate_pin();
        if !state.games.contains_key(&candidate) {
            break candidate;
        }
    };

    state.games.insert(pin.clone(), GameSession::new(pin.clone(), quiz_data));

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
    if !state.games.contains_key(&pin) {
        return Err(StatusCode::NOT_FOUND);
    }

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

    // Register host — build message inside guard, send after dropping it
    let lobby_msg = {
        let Some(mut session) = state.games.get_mut(&pin) else {
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

        let player_list: Vec<_> = session
            .players
            .values()
            .map(|p| json!({ "nickname": p.nickname, "avatar": p.avatar }))
            .collect();
        json!({
            "type": "lobby",
            "pin": pin,
            "quiz_title": session.quiz.title,
            "players": player_list,
            "background_url": session.quiz.background_url,
            "music_url": session.quiz.music_url,
        })
        .to_string()
        // guard dropped here
    };
    let _ = ws_sender
        .send(Message::Text(lobby_msg.into()))
        .await;

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
                if let Some(mut session) = state.games.get_mut(&pin)
                    && session.phase == GamePhase::Lobby {
                        if let Some(mode) = parsed["streak_mode"].as_str() {
                            session.streak_mode = StreakMode::parse(mode);
                        }
                        start_question(&mut session, &state, &pin);
                    }
            }
            "next" => {
                if let Some(mut session) = state.games.get_mut(&pin)
                    && matches!(session.phase, GamePhase::Results | GamePhase::Slide) {
                        if session.current_question + 1 < session.quiz.questions.len() {
                            session.current_question += 1;
                            start_question(&mut session, &state, &pin);
                        } else {
                            finish_game(&mut session);
                        }
                    }
            }
            _ => {}
        }
    }

    // Host disconnected — tear down game
    send_task.abort();
    if let Some((_, session)) = state.games.remove(&pin) {
        session.send_to_all_players(
            &json!({"type": "game_over", "reason": "Host disconnected"}).to_string(),
        );
    }
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
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text)
                    && parsed["type"].as_str() == Some("join")
                        && let Some(nick) = parsed["nickname"].as_str()
                            && !nick.trim().is_empty() {
                                break nick.trim().to_string();
                            }
            }
            _ => return,
        }
    };

    // Register player — build message inside guard, send after dropping it
    let join_msg = {
        let Some(mut session) = state.games.get_mut(&pin) else {
            let _ = ws_sender
                .send(Message::Text(
                    json!({"type": "error", "message": "Game not found"})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        };

        let started = session.phase != GamePhase::Lobby;

        // Mid-game: only a previously-disconnected player (matched by nickname)
        // may rejoin, resuming with their score, original avatar, and streak.
        // Fresh joins are rejected.
        let (score, avatar, streak) = if started {
            match session.disconnected.remove(&nickname) {
                Some(state) => state,
                None => {
                    let msg =
                        json!({"type": "error", "message": "Game already started"}).to_string();
                    drop(session); // drop guard before await
                    let _ = ws_sender.send(Message::Text(msg.into())).await;
                    return;
                }
            }
        } else {
            (0, game::pick_avatar(), 0)
        };

        session.players.insert(
            player_id.clone(),
            Player {
                nickname: nickname.clone(),
                avatar: avatar.clone(),
                score,
                streak,
                tx,
            },
        );

        let player_count = session.players.len();
        session.send_to_host(
            &json!({
                "type": "player_joined",
                "nickname": nickname,
                "avatar": avatar,
                "player_count": player_count,
            })
            .to_string(),
        );

        if started {
            // Reconnecting mid-game: sync the player to the current phase.
            player_state_msg(&session, &player_id)
        } else {
            let bg = session.quiz.background_url.clone();
            json!({
                "type": "joined",
                "message": "Waiting for host to start the game...",
                "background_url": bg,
                "avatar": avatar,
            })
            .to_string()
        }
        // guard dropped here
    };
    let _ = ws_sender.send(Message::Text(join_msg.into())).await;

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

        if msg_type == "answer"
            && let Some(index) = parsed["index"].as_u64()
                && let Some(mut session) = state.games.get_mut(&pin)
                    && session.phase == GamePhase::Question
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
                        let n_opts = session.quiz.questions[session.current_question]
                            .answers
                            .len();
                        let mut counts = vec![0usize; n_opts];
                        for a in session.answers.values() {
                            if a.answer_index < counts.len() {
                                counts[a.answer_index] += 1;
                            }
                        }
                        session.send_to_host(
                            &json!({
                                "type": "answer_count",
                                "count": count,
                                "total": total,
                                "counts": counts,
                            })
                            .to_string(),
                        );

                        if let Some(player) = session.players.get(&player_id) {
                            let _ = player.tx.send(
                                json!({"type": "answer_accepted"}).to_string(),
                            );
                        }

                        if session.all_answered() {
                            close_question(&mut session);
                        }
                    }

        if msg_type == "numeric_answer"
            && let Some(value) = parsed["value"].as_f64()
            && value.is_finite()
            && let Some(mut session) = state.games.get_mut(&pin)
            && session.phase == GamePhase::Numeric
            && !session.numeric_answers.contains_key(&player_id)
        {
            let time_ms = session
                .question_started_at
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0);
            session
                .numeric_answers
                .insert(player_id.clone(), (value, time_ms));

            let count = session.numeric_answers.len();
            let total = session.players.len();
            session.send_to_host(
                &json!({ "type": "answer_count", "count": count, "total": total }).to_string(),
            );
            if let Some(player) = session.players.get(&player_id) {
                let _ = player.tx.send(json!({"type": "answer_accepted"}).to_string());
            }
            if count >= total {
                close_numeric(&mut session);
            }
        }

        if msg_type == "open_answer"
            && let Some(text) = parsed["text"].as_str()
            && let Some(mut session) = state.games.get_mut(&pin)
            && session.phase == GamePhase::OpenAnswer
            && !session.open_answers.contains_key(&player_id)
        {
            let text: String = text.trim().chars().take(200).collect();
            if !text.is_empty() {
                session.open_answers.insert(player_id.clone(), text);

                let count = session.open_answers.len();
                let total = session.players.len();
                session.send_to_host(
                    &json!({ "type": "answer_count", "count": count, "total": total }).to_string(),
                );
                if let Some(player) = session.players.get(&player_id) {
                    let _ = player.tx.send(json!({"type": "answer_accepted"}).to_string());
                }

                if count >= total {
                    start_voting(&mut session, &state.games, &pin);
                }
            }
        }

        if msg_type == "vote"
            && let Some(index) = parsed["index"].as_u64()
            && let Some(mut session) = state.games.get_mut(&pin)
            && session.phase == GamePhase::Voting
            && !session.votes.contains_key(&player_id)
        {
            let idx = index as usize;
            // Must be a valid option and not the player's own answer.
            if idx < session.vote_options.len() && session.vote_options[idx].0 != player_id {
                session.votes.insert(player_id.clone(), idx);

                let count = session.votes.len();
                let total = session.players.len();
                session.send_to_host(
                    &json!({ "type": "answer_count", "count": count, "total": total }).to_string(),
                );
                if let Some(player) = session.players.get(&player_id) {
                    let _ = player.tx.send(json!({"type": "answer_accepted"}).to_string());
                }

                if count >= total {
                    finalize_open(&mut session);
                }
            }
        }

        // Lobby reactions: players tap an emoji from a whitelist; the host
        // overlays floating emojis on the lobby. Whitelisted to prevent spam.
        if msg_type == "reaction"
            && let Some(emoji) = parsed["emoji"].as_str()
            && is_allowed_reaction(emoji)
            && let Some(session) = state.games.get(&pin)
            && session.phase == GamePhase::Lobby
        {
            let nickname = session
                .players
                .get(&player_id)
                .map(|p| p.nickname.clone())
                .unwrap_or_default();
            session.send_to_host(
                &json!({
                    "type": "reaction",
                    "emoji": emoji,
                    "nickname": nickname,
                })
                .to_string(),
            );
        }
    }

    // Player disconnected
    send_task.abort();
    if let Some(mut session) = state.games.get_mut(&pin) {
        if let Some(player) = session.players.remove(&player_id) {
            // Mid-game: keep score, avatar, and streak so the player can reconnect by nickname.
            if session.phase != GamePhase::Lobby {
                session.disconnected.insert(
                    player.nickname,
                    (player.score, player.avatar, player.streak),
                );
            }
        }
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
            close_question(&mut session);
        }
        if session.phase == GamePhase::Numeric
            && !session.players.is_empty()
            && session.numeric_answers.len() >= session.players.len()
        {
            close_numeric(&mut session);
        }
    }
}

// --- Game flow helpers ---

fn start_question(session: &mut GameSession, state: &AppState, pin: &str) {
    session.answers.clear();
    session.numeric_answers.clear();
    session.open_answers.clear();
    session.vote_options.clear();
    session.votes.clear();

    let idx = session.current_question;
    let total = session.quiz.questions.len();
    let is_last = idx + 1 >= total;
    let kind = session.quiz.questions[idx].kind.clone();

    // Section/content slide: show it, no timer, no scoring; host advances manually.
    if kind == "slide" {
        session.phase = GamePhase::Slide;
        session.question_started_at = None;

        let q = &session.quiz.questions[idx];
        let msg = json!({
            "type": "slide",
            "index": idx,
            "total": total,
            "text": q.text,
            "image_url": q.image_url,
            "is_last": is_last,
        })
        .to_string();
        session.send_to_host(&msg);
        session.send_to_all_players(&msg);
        return;
    }

    // Open-answer: players type free text, then vote.
    if kind == "open" {
        start_open(session, state, pin, idx, total);
        return;
    }

    // Numeric / closest-wins: players type a number.
    if kind == "numeric" {
        start_numeric(session, state, pin, idx, total);
        return;
    }

    session.phase = GamePhase::Question;
    session.question_started_at = Some(std::time::Instant::now());

    let q = &session.quiz.questions[session.current_question];

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
        if let Some(mut session) = games.get_mut(&pin)
            && session.phase == GamePhase::Question && session.current_question == question_idx {
                close_question(&mut session);
            }
    });
}

fn close_question(session: &mut GameSession) {
    // Snapshot scores before this round for delta calculation
    let prev_scores: HashMap<String, i64> = session
        .players
        .iter()
        .map(|(id, p)| (id.clone(), p.score))
        .collect();

    session.phase = GamePhase::Results;
    let q = &session.quiz.questions[session.current_question];
    let time_limit_ms = q.time_limit_secs as u64 * 1000;
    let streak_mode = session.streak_mode;

    // 1. Compute correctness, base points (time-bonus only), and timing per answerer.
    let mut player_base = HashMap::<String, (bool, i64, u64)>::new();
    let mut answer_times: Vec<u64> = Vec::new();

    for (player_id, answer) in &session.answers {
        let correct = q
            .answers
            .get(answer.answer_index)
            .map(|a| a.is_correct)
            .unwrap_or(false);

        let base_points = if correct && time_limit_ms > 0 {
            let time_taken = answer.time_ms.min(time_limit_ms);
            (1000 - (500 * time_taken / time_limit_ms)) as i64
        } else {
            0
        };

        answer_times.push(answer.time_ms);
        player_base.insert(player_id.clone(), (correct, base_points, answer.time_ms));
    }

    // 2. Walk every player (including those who didn't answer) to update streaks
    //    and apply the streak multiplier when active. The final tuple per player
    //    is (correct, base_points, awarded_points, streak_after, time_ms).
    let mut player_results = HashMap::<String, (bool, i64, i64, u32, u64)>::new();
    for (player_id, player) in &mut session.players {
        let (correct, base_points, time_ms) =
            player_base.get(player_id).copied().unwrap_or((false, 0, 0));

        if correct {
            player.streak += 1;
        } else {
            player.streak = 0;
        }

        let awarded = if streak_mode == StreakMode::Multiplier && correct {
            let m = game::streak_multiplier(player.streak);
            ((base_points as f64) * m).round() as i64
        } else {
            base_points
        };

        player.score += awarded;
        player_results.insert(
            player_id.clone(),
            (correct, base_points, awarded, player.streak, time_ms),
        );
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

    // Host results — look up previous score by player ID directly
    session.send_to_host(
        &json!({
            "type": "results",
            "answers": q.answers.iter().enumerate().map(|(i, a)| json!({
                "text": a.text,
                "is_correct": a.is_correct,
                "count": answer_counts[i],
            })).collect::<Vec<_>>(),
            "leaderboard": leaderboard.iter().map(|e| {
                let prev = prev_scores.iter()
                    .find(|(id, _)| session.players.get(*id).map(|p| p.nickname == e.nickname).unwrap_or(false))
                    .map(|(_, &s)| s)
                    .unwrap_or(0);
                json!({
                    "nickname": e.nickname,
                    "avatar": e.avatar,
                    "score": e.score,
                    "gained": e.score - prev,
                })
            }).collect::<Vec<_>>(),
            "is_last": is_last,
            "fastest_ms": fastest_ms,
        })
        .to_string(),
    );

    // Text of the correct option(s) — joined with " / " when more than one is correct.
    let correct_answer_text: String = q
        .answers
        .iter()
        .filter(|a| a.is_correct)
        .map(|a| a.text.clone())
        .collect::<Vec<_>>()
        .join(" / ");

    // Individual results to each player — include timing stats and the picked answer text.
    for (player_id, player) in &session.players {
        let (correct, base_points, awarded, streak, time_ms) = player_results
            .get(player_id)
            .copied()
            .unwrap_or((false, 0, 0, 0, 0));
        let rank = leaderboard
            .iter()
            .position(|e| e.nickname == player.nickname)
            .unwrap_or(0)
            + 1;
        let speed_rank = answer_times.iter().filter(|&&t| t < time_ms).count() + 1;
        let answered = player_base.contains_key(player_id);
        let your_answer = session
            .answers
            .get(player_id)
            .and_then(|a| q.answers.get(a.answer_index))
            .map(|a| a.text.clone());

        let _ = player.tx.send(
            json!({
                "type": "result",
                "correct": correct,
                "points": awarded,
                "base_points": base_points,
                "streak": streak,
                "streak_mode": streak_mode.as_str(),
                "score": player.score,
                "rank": rank,
                "total_players": session.players.len(),
                "time_ms": if answered { time_ms } else { 0 },
                "speed_rank": if answered { speed_rank } else { 0 },
                "fastest_ms": fastest_ms,
                "average_ms": average_ms,
                "total_answered": answer_times.len(),
                "your_answer": your_answer,
                "correct_answer": correct_answer_text,
            })
            .to_string(),
        );
    }
}

// --- Numeric / closest-wins flow ---

/// Points awarded by finishing rank (closest distance to the correct value).
/// Rank index 0 → 1st place, 1 → 2nd, 2 → 3rd. Time bonus shrinks them
/// proportionally for slower submissions, same shape as the MCQ scoring.
const NUMERIC_RANK_POINTS: &[i64] = &[1000, 500, 250];

fn start_numeric(session: &mut GameSession, state: &AppState, pin: &str, idx: usize, total: usize) {
    session.phase = GamePhase::Numeric;
    session.question_started_at = Some(std::time::Instant::now());

    let q = &session.quiz.questions[idx];
    let msg = json!({
        "type": "numeric_question",
        "index": idx,
        "total": total,
        "text": q.text,
        "image_url": q.image_url,
        "time_limit": q.time_limit_secs,
    })
    .to_string();
    session.send_to_host(&msg);
    session.send_to_all_players(&msg);

    // Auto-close after the time limit.
    let games = state.games.clone();
    let pin = pin.to_string();
    let question_idx = idx;
    let time_limit = q.time_limit_secs;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(time_limit as u64)).await;
        if let Some(mut session) = games.get_mut(&pin)
            && session.phase == GamePhase::Numeric
            && session.current_question == question_idx
        {
            close_numeric(&mut session);
        }
    });
}

fn close_numeric(session: &mut GameSession) {
    let prev_scores: HashMap<String, i64> = session
        .players
        .iter()
        .map(|(id, p)| (id.clone(), p.score))
        .collect();

    session.phase = GamePhase::Results;
    let q = &session.quiz.questions[session.current_question];
    let time_limit_ms = q.time_limit_secs as u64 * 1000;
    let streak_mode = session.streak_mode;

    // Parse the correct value from the first answer row. If unparseable, nobody scores.
    let correct: Option<f64> = q
        .answers
        .first()
        .and_then(|a| a.text.trim().parse::<f64>().ok());

    // (player_id, value, time_ms, error)
    let mut ordered: Vec<(String, f64, u64, f64)> = Vec::new();
    if let Some(c) = correct {
        for (pid, (v, t)) in &session.numeric_answers {
            ordered.push((pid.clone(), *v, *t, (*v - c).abs()));
        }
    }
    // Smallest error first; ties broken by faster time.
    ordered.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal)
        .then(a.2.cmp(&b.2)));

    // Map player_id -> (rank index, base_points). Awarded only to top-3.
    let mut rank_map = HashMap::<String, (usize, i64)>::new();
    for (i, (pid, _v, t, _e)) in ordered.iter().enumerate() {
        let base = NUMERIC_RANK_POINTS.get(i).copied().unwrap_or(0);
        let base = if base > 0 && time_limit_ms > 0 {
            let time_taken = (*t).min(time_limit_ms);
            // Same shape as MCQ: keep at least half, lose up to half to time.
            ((base as f64) * (1.0 - 0.5 * (time_taken as f64) / (time_limit_ms as f64))).round()
                as i64
        } else {
            base
        };
        rank_map.insert(pid.clone(), (i, base));
    }

    // Update streaks + apply multiplier, then award.
    let mut player_results = HashMap::<String, (bool, i64, i64, u32, usize, Option<f64>, u64)>::new();
    for (pid, player) in &mut session.players {
        let answered = session.numeric_answers.contains_key(pid);
        let your_value = session.numeric_answers.get(pid).map(|(v, _)| *v);
        let your_time = session.numeric_answers.get(pid).map(|(_, t)| *t).unwrap_or(0);
        let (rank_idx, base_points) = rank_map.get(pid).copied().unwrap_or((usize::MAX, 0));
        let got_points = base_points > 0;

        if got_points {
            player.streak += 1;
        } else if answered {
            player.streak = 0;
        }
        // Players who didn't answer at all leave their streak alone — same as for
        // non-MCQ phases — to avoid penalising someone who lagged out.

        let awarded = if streak_mode == StreakMode::Multiplier && got_points {
            let m = game::streak_multiplier(player.streak);
            ((base_points as f64) * m).round() as i64
        } else {
            base_points
        };

        player.score += awarded;
        player_results.insert(
            pid.clone(),
            (got_points, base_points, awarded, player.streak, rank_idx, your_value, your_time),
        );
    }

    let leaderboard = session.leaderboard();
    let is_last = session.current_question + 1 >= session.quiz.questions.len();

    // Host results: list of all submissions sorted closest→farthest.
    session.send_to_host(
        &json!({
            "type": "numeric_results",
            "correct": correct,
            "submissions": ordered.iter().enumerate().map(|(i, (pid, v, _t, err))| {
                let nick = session.players.get(pid).map(|p| p.nickname.clone()).unwrap_or_default();
                let avatar = session.players.get(pid).map(|p| p.avatar.clone()).unwrap_or_default();
                let base = NUMERIC_RANK_POINTS.get(i).copied().unwrap_or(0);
                json!({
                    "rank": i + 1,
                    "nickname": nick,
                    "avatar": avatar,
                    "value": v,
                    "error": err,
                    "rank_points": base,
                })
            }).collect::<Vec<_>>(),
            "leaderboard": leaderboard.iter().map(|e| {
                let prev = prev_scores.iter()
                    .find(|(id, _)| session.players.get(*id).map(|p| p.nickname == e.nickname).unwrap_or(false))
                    .map(|(_, &s)| s)
                    .unwrap_or(0);
                json!({
                    "nickname": e.nickname,
                    "avatar": e.avatar,
                    "score": e.score,
                    "gained": e.score - prev,
                })
            }).collect::<Vec<_>>(),
            "is_last": is_last,
        })
        .to_string(),
    );

    // Per-player result.
    for (pid, player) in &session.players {
        let (got_points, base_points, awarded, streak, rank_idx, your_value, time_ms) =
            player_results.get(pid).copied().unwrap_or((false, 0, 0, 0, usize::MAX, None, 0));
        let lb_rank = leaderboard
            .iter()
            .position(|e| e.nickname == player.nickname)
            .unwrap_or(0)
            + 1;
        let answered = your_value.is_some();
        let your_rank: Option<usize> = if answered { Some(rank_idx + 1) } else { None };

        let _ = player.tx.send(
            json!({
                "type": "numeric_result",
                "correct_answer": correct,
                "your_value": your_value,
                "your_rank": your_rank,
                "total_submissions": ordered.len(),
                "got_points": got_points,
                "base_points": base_points,
                "points": awarded,
                "streak": streak,
                "streak_mode": streak_mode.as_str(),
                "score": player.score,
                "rank": lb_rank,
                "total_players": session.players.len(),
                "time_ms": if answered { time_ms } else { 0 },
            })
            .to_string(),
        );
    }
}

// --- Open-answer flow ---

fn start_open(session: &mut GameSession, state: &AppState, pin: &str, idx: usize, total: usize) {
    session.phase = GamePhase::OpenAnswer;
    session.question_started_at = Some(std::time::Instant::now());

    let q = &session.quiz.questions[idx];
    let msg = json!({
        "type": "open_question",
        "index": idx,
        "total": total,
        "text": q.text,
        "image_url": q.image_url,
        "time_limit": q.time_limit_secs,
    })
    .to_string();
    session.send_to_host(&msg);
    session.send_to_all_players(&msg);

    // Auto-advance to voting after the time limit.
    let games = state.games.clone();
    let pin = pin.to_string();
    let question_idx = idx;
    let time_limit = q.time_limit_secs;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(time_limit as u64)).await;
        if let Some(mut session) = games.get_mut(&pin)
            && session.phase == GamePhase::OpenAnswer
            && session.current_question == question_idx
        {
            start_voting(&mut session, &games, &pin);
        }
    });
}

fn start_voting(session: &mut GameSession, games: &game::GameManager, pin: &str) {
    // Build vote options from non-empty open answers, ordered deterministically.
    let mut options: Vec<(String, String)> = session
        .open_answers
        .iter()
        .filter(|(_, t)| !t.trim().is_empty())
        .map(|(id, t)| (id.clone(), t.trim().to_string()))
        .collect();
    options.sort_by(|a, b| a.1.cmp(&b.1));
    session.vote_options = options;
    session.votes.clear();

    // Need at least two answers to hold a vote; otherwise skip to results.
    if session.vote_options.len() < 2 {
        finalize_open(session);
        return;
    }

    session.phase = GamePhase::Voting;
    session.question_started_at = Some(std::time::Instant::now());

    let texts: Vec<String> = session.vote_options.iter().map(|(_, t)| t.clone()).collect();
    let total = session.players.len();

    session.send_to_host(
        &json!({ "type": "voting", "options": texts, "total": total }).to_string(),
    );

    // Each player gets the option list and the index of their own answer (disabled).
    for (pid, player) in &session.players {
        let own = session.vote_options.iter().position(|(aid, _)| aid == pid);
        let _ = player.tx.send(
            json!({ "type": "voting", "options": texts, "own_index": own }).to_string(),
        );
    }

    // Auto-close voting after a fixed window.
    let games = games.clone();
    let pin = pin.to_string();
    let question_idx = session.current_question;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(VOTE_TIME_SECS)).await;
        if let Some(mut session) = games.get_mut(&pin)
            && session.phase == GamePhase::Voting
            && session.current_question == question_idx
        {
            finalize_open(&mut session);
        }
    });
}

fn finalize_open(session: &mut GameSession) {
    let prev_scores: HashMap<String, i64> = session
        .players
        .iter()
        .map(|(id, p)| (id.clone(), p.score))
        .collect();
    session.phase = GamePhase::Results;

    let options = session.vote_options.clone();
    let n = options.len();
    let votes: Vec<(String, usize)> =
        session.votes.iter().map(|(k, &v)| (k.clone(), v)).collect();

    let mut counts = vec![0usize; n];
    for (_, idx) in &votes {
        if *idx < n {
            counts[*idx] += 1;
        }
    }
    let max = counts.iter().copied().max().unwrap_or(0);

    // Authors earn 100 per vote received.
    for (i, (author_id, _)) in options.iter().enumerate() {
        let pts = counts[i] as i64 * 100;
        if pts > 0 {
            if let Some(p) = session.players.get_mut(author_id) {
                p.score += pts;
            }
        }
    }
    // Voters who picked a top answer earn a bonus.
    if max > 0 {
        for (voter_id, idx) in &votes {
            if counts[*idx] == max {
                if let Some(p) = session.players.get_mut(voter_id) {
                    p.score += 500;
                }
            }
        }
    }

    let leaderboard = session.leaderboard();
    let is_last = session.current_question + 1 >= session.quiz.questions.len();

    // Host results reuse the choice-results shape (top answers marked correct).
    session.send_to_host(
        &json!({
            "type": "results",
            "answers": options.iter().enumerate().map(|(i, (_, t))| json!({
                "text": t,
                "is_correct": max > 0 && counts[i] == max,
                "count": counts[i],
            })).collect::<Vec<_>>(),
            "leaderboard": leaderboard.iter().map(|e| {
                let prev = prev_scores.iter()
                    .find(|(id, _)| session.players.get(*id).map(|p| p.nickname == e.nickname).unwrap_or(false))
                    .map(|(_, &s)| s)
                    .unwrap_or(0);
                json!({ "nickname": e.nickname, "avatar": e.avatar, "score": e.score, "gained": e.score - prev })
            }).collect::<Vec<_>>(),
            "is_last": is_last,
            "fastest_ms": 0,
        })
        .to_string(),
    );

    // Individual player results reuse the choice result shape.
    for (player_id, player) in &session.players {
        let prev = prev_scores.get(player_id).copied().unwrap_or(0);
        let gained = player.score - prev;
        let rank = leaderboard
            .iter()
            .position(|e| e.nickname == player.nickname)
            .unwrap_or(0)
            + 1;
        let _ = player.tx.send(
            json!({
                "type": "result",
                "correct": gained > 0,
                "points": gained,
                "score": player.score,
                "rank": rank,
                "total_players": session.players.len(),
                "time_ms": 0,
                "speed_rank": 0,
                "fastest_ms": 0,
                "average_ms": 0,
                "total_answered": votes.len(),
            })
            .to_string(),
        );
    }
}

/// Build the message that re-syncs a reconnecting player to the current phase.
fn player_state_msg(session: &GameSession, player_id: &str) -> String {
    let idx = session.current_question;
    let total = session.quiz.questions.len();
    match session.phase {
        GamePhase::Question => {
            let q = &session.quiz.questions[idx];
            json!({
                "type": "question",
                "index": idx,
                "total": total,
                "text": q.text,
                "image_url": q.image_url,
                "answers": q.answers.iter().map(|a| a.text.clone()).collect::<Vec<_>>(),
                "time_limit": q.time_limit_secs,
            })
            .to_string()
        }
        GamePhase::Slide => {
            let q = &session.quiz.questions[idx];
            json!({
                "type": "slide",
                "index": idx,
                "total": total,
                "text": q.text,
                "image_url": q.image_url,
                "is_last": idx + 1 >= total,
            })
            .to_string()
        }
        GamePhase::OpenAnswer => {
            let q = &session.quiz.questions[idx];
            json!({
                "type": "open_question",
                "index": idx,
                "total": total,
                "text": q.text,
                "image_url": q.image_url,
                "time_limit": q.time_limit_secs,
            })
            .to_string()
        }
        GamePhase::Numeric => {
            let q = &session.quiz.questions[idx];
            json!({
                "type": "numeric_question",
                "index": idx,
                "total": total,
                "text": q.text,
                "image_url": q.image_url,
                "time_limit": q.time_limit_secs,
            })
            .to_string()
        }
        GamePhase::Voting => {
            let texts: Vec<String> = session.vote_options.iter().map(|(_, t)| t.clone()).collect();
            let own = session.vote_options.iter().position(|(aid, _)| aid == player_id);
            json!({ "type": "voting", "options": texts, "own_index": own }).to_string()
        }
        GamePhase::Finished => {
            let leaderboard = session.leaderboard();
            let (rank, score) = session
                .players
                .get(player_id)
                .map(|p| {
                    let r = leaderboard
                        .iter()
                        .position(|e| e.nickname == p.nickname)
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    (r, p.score)
                })
                .unwrap_or((0, 0));
            json!({ "type": "finished", "rank": rank, "score": score, "leaderboard": leaderboard })
                .to_string()
        }
        // Lobby or Results: show the waiting screen until the next event.
        _ => json!({
            "type": "joined",
            "message": "Waiting for the next question…",
            "background_url": session.quiz.background_url,
        })
        .to_string(),
    }
}

fn finish_game(session: &mut GameSession) {
    session.phase = GamePhase::Finished;
    let leaderboard = session.leaderboard();

    session.send_to_host(&json!({"type": "finished", "leaderboard": leaderboard}).to_string());

    for player in session.players.values() {
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
