use dashmap::DashMap;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

pub type GameManager = Arc<DashMap<String, GameSession>>;

pub fn new_manager() -> GameManager {
    Arc::new(DashMap::new())
}

pub fn generate_pin() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    format!("{:06}", rng.gen_range(0..1_000_000))
}

#[derive(Clone)]
pub struct QuizData {
    pub title: String,
    pub questions: Vec<QuestionData>,
    pub background_url: Option<String>,
    pub music_url: Option<String>,
}

#[derive(Clone)]
pub struct QuestionData {
    pub text: String,
    pub answers: Vec<AnswerChoice>,
    pub time_limit_secs: i32,
    pub image_url: Option<String>,
}

#[derive(Clone)]
pub struct AnswerChoice {
    pub text: String,
    pub is_correct: bool,
}

pub struct Player {
    pub nickname: String,
    pub score: i64,
    pub tx: mpsc::UnboundedSender<String>,
}

pub struct PlayerAnswer {
    pub answer_index: usize,
    pub time_ms: u64,
}

#[derive(PartialEq, Clone, Copy)]
pub enum GamePhase {
    Lobby,
    Question,
    Results,
    Finished,
}

pub struct GameSession {
    pub pin: String,
    pub quiz: QuizData,
    pub host_tx: Option<mpsc::UnboundedSender<String>>,
    pub players: HashMap<String, Player>,
    pub phase: GamePhase,
    pub current_question: usize,
    pub question_started_at: Option<Instant>,
    pub answers: HashMap<String, PlayerAnswer>,
}

impl GameSession {
    pub fn new(pin: String, quiz: QuizData) -> Self {
        Self {
            pin,
            quiz,
            host_tx: None,
            players: HashMap::new(),
            phase: GamePhase::Lobby,
            current_question: 0,
            question_started_at: None,
            answers: HashMap::new(),
        }
    }

    pub fn all_answered(&self) -> bool {
        !self.players.is_empty() && self.answers.len() >= self.players.len()
    }

    pub fn send_to_host(&self, msg: &str) {
        if let Some(tx) = &self.host_tx {
            let _ = tx.send(msg.to_string());
        }
    }

    pub fn send_to_all_players(&self, msg: &str) {
        for player in self.players.values() {
            let _ = player.tx.send(msg.to_string());
        }
    }

    pub fn leaderboard(&self) -> Vec<LeaderboardEntry> {
        let mut entries: Vec<_> = self
            .players.values().map(|p| LeaderboardEntry {
                nickname: p.nickname.clone(),
                score: p.score,
            })
            .collect();
        entries.sort_by(|a, b| b.score.cmp(&a.score));
        entries
    }
}

#[derive(Serialize, Clone)]
pub struct LeaderboardEntry {
    pub nickname: String,
    pub score: i64,
}
