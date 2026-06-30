-- Optional email for account recovery (nullable; existing users have none).
-- Multiple NULLs are allowed under a Postgres UNIQUE constraint.
ALTER TABLE users ADD COLUMN email TEXT UNIQUE;

-- Password reset tokens. We store only the SHA-256 hash of the token, never the
-- raw value, so a DB leak does not hand out working reset links.
CREATE TABLE password_reset_tokens (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_password_reset_tokens_user ON password_reset_tokens(user_id);
