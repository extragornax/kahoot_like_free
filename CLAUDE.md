# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Kahoot-like quiz server in Rust (edition 2024). Players can create and play quizzes with friends in real-time.

## Build & Run

```bash
cargo build          # compile
cargo run            # run the server
cargo test           # run all tests
cargo test <name>    # run a single test by name
cargo clippy         # lint
cargo fmt --check    # check formatting
cargo fmt            # auto-format
```

## Docker

```bash
docker compose up -d          # start db + app (with cargo-watch auto-reload)
docker compose logs -f app    # follow app logs
docker compose down           # stop everything
```

- **PostgreSQL** internal to Docker network (port 5432), credentials: `kahoot/kahoot`, database: `kahoot`
- **App** connects via `DATABASE_URL=postgres://kahoot:kahoot@db:5432/kahoot` (internal docker network)
- Source is volume-mounted — `cargo-watch` recompiles on file changes
- For local dev outside Docker: run `docker compose up -d db` first, then use `DATABASE_URL="postgres://kahoot:kahoot@db:5432/kahoot" cargo run` (requires the app to be on the Docker network, or use `docker compose up` instead)

## Architecture

**Stack:** Axum (HTTP + WebSocket) / SQLx (async Postgres, compile-time query checking) / Argon2 (password hashing) / JWT (auth tokens)

**Two web interfaces:**
- `static/index.html` — computer screen (served at `/`): quiz management, game control, leaderboard display
- `static/player.html` — phone screen: join via QR code, answer questions

**Code layout:**
- `src/main.rs` — server startup, route wiring, AppState
- `src/auth.rs` — JWT creation/verification, `AuthUser` extractor
- `src/models.rs` — DB row structs + API request/response types
- `src/game.rs` — in-memory game state (GameManager, GameSession, scoring, PIN generation)
- `src/handlers/` — route handlers (auth, quiz CRUD, game WebSocket)
- `migrations/` — SQLx SQL migrations (auto-run on startup via `sqlx::migrate!()`)

**API routes:** all under `/api`
- `POST /api/auth/register`, `POST /api/auth/login` — public
- `GET/POST /api/quizzes`, `GET/DELETE /api/quizzes/{id}` — requires `Authorization: Bearer <token>`
- `POST /api/games/{quiz_id}` — create a game session, returns `{pin}` (auth required)

**WebSocket routes:**
- `GET /ws/host/{pin}` — host connects to control the game
- `GET /ws/play/{pin}` — player connects to join and play
