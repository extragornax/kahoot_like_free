-- Persisted results of finished games, written when the host ends the final
-- question. Quiz title/question text are denormalized so history survives
-- quiz edits and deletions.

CREATE TABLE game_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    host_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    quiz_id UUID REFERENCES quizzes(id) ON DELETE SET NULL,
    quiz_title TEXT NOT NULL,
    question_count INT NOT NULL,
    player_count INT NOT NULL,
    finished_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE game_history_players (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    game_id UUID NOT NULL REFERENCES game_history(id) ON DELETE CASCADE,
    rank INT NOT NULL,
    nickname TEXT NOT NULL,
    avatar TEXT NOT NULL,
    score BIGINT NOT NULL,
    correct_count INT NOT NULL,
    best_streak INT NOT NULL
);

CREATE TABLE game_history_questions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    game_id UUID NOT NULL REFERENCES game_history(id) ON DELETE CASCADE,
    position INT NOT NULL,
    text TEXT NOT NULL,
    kind TEXT NOT NULL,
    answered_count INT NOT NULL,
    -- NULL for open questions, where answers are voted on rather than correct.
    correct_count INT,
    player_count INT NOT NULL
);

CREATE INDEX idx_game_history_host ON game_history(host_id, finished_at DESC);
CREATE INDEX idx_game_history_players_game ON game_history_players(game_id);
CREATE INDEX idx_game_history_questions_game ON game_history_questions(game_id);
