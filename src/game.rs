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

/// Pool of friendly avatar emojis assigned to players on join.
const AVATAR_POOL: &[&str] = &[
    "🦊", "🐱", "🐶", "🐼", "🦁", "🐯", "🐸", "🐵", "🦄", "🐧", "🐨", "🐰",
    "🐻", "🐮", "🐷", "🦝", "🦒", "🦔", "🦦", "🐢", "🐳", "🐙", "🦋", "🐝",
    "🍕", "🍔", "🌮", "🍦", "🍩", "🍓", "🍉", "🍒", "🥑", "🍍", "🥥",
    "🚀", "⚡", "🔥", "⭐", "🌟", "💎", "🎩", "👑", "🎯", "🎨", "🎸", "🎲",
];

pub fn pick_avatar() -> String {
    use rand::seq::SliceRandom;
    AVATAR_POOL
        .choose(&mut rand::thread_rng())
        .copied()
        .unwrap_or("🙂")
        .to_string()
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
    pub kind: String,
}

#[derive(Clone)]
pub struct AnswerChoice {
    pub text: String,
    pub is_correct: bool,
}

pub struct Player {
    pub nickname: String,
    pub avatar: String,
    pub score: i64,
    /// Current run of consecutive correct multi-choice answers. Resets on a
    /// wrong/missed multi-choice answer; other question kinds leave it alone.
    pub streak: u32,
    pub tx: mpsc::UnboundedSender<String>,
}

/// How streaks of consecutive correct answers affect the game.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum StreakMode {
    /// No streak tracking at all.
    Off,
    /// Track the streak and surface it to the player as a flame badge,
    /// but don't change the scoring.
    AnimationsOnly,
    /// Track the streak and multiply earned points by `streak_multiplier`.
    Multiplier,
}

impl StreakMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "multiplier" => StreakMode::Multiplier,
            "animations" | "animations_only" | "animation" => StreakMode::AnimationsOnly,
            _ => StreakMode::Off,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            StreakMode::Off => "off",
            StreakMode::AnimationsOnly => "animations",
            StreakMode::Multiplier => "multiplier",
        }
    }
}

/// Points multiplier earned at a given streak length.
/// streak ≤ 1 → 1.0x (no bonus on the first correct of a run),
/// then +0.25x per additional correct, capped at 2.0x for streak ≥ 5.
pub fn streak_multiplier(streak: u32) -> f64 {
    if streak <= 1 {
        return 1.0;
    }
    let levels = (streak - 1).min(4);
    1.0 + 0.25 * levels as f64
}

pub struct PlayerAnswer {
    pub answer_index: usize,
    pub time_ms: u64,
}

#[derive(PartialEq, Clone, Copy)]
pub enum GamePhase {
    Lobby,
    Question,
    /// A content/section slide: no answers, no scoring, host advances manually.
    Slide,
    /// Open-answer question: players type a free-text answer.
    OpenAnswer,
    /// Voting on the open answers collected in the OpenAnswer phase.
    Voting,
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
    /// Open-answer submissions for the current question: player_id -> text.
    pub open_answers: HashMap<String, String>,
    /// Voting options built from open answers: (author_player_id, text), index = vote target.
    pub vote_options: Vec<(String, String)>,
    /// Votes cast: voter_player_id -> vote_options index.
    pub votes: HashMap<String, usize>,
    /// Score, avatar, and streak of players who dropped mid-game, kept by
    /// nickname so a reconnect can restore all three.
    pub disconnected: HashMap<String, (i64, String, u32)>,
    /// How streaks are tracked for this game (chosen by the host in the lobby).
    pub streak_mode: StreakMode,
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
            open_answers: HashMap::new(),
            vote_options: Vec::new(),
            votes: HashMap::new(),
            disconnected: HashMap::new(),
            streak_mode: StreakMode::Off,
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
                avatar: p.avatar.clone(),
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
    pub avatar: String,
    pub score: i64,
}
