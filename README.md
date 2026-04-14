# Kahoot Free

A self-hosted, real-time quiz game platform inspired by Kahoot. Built in Rust with Axum, WebSockets, and PostgreSQL.

Host a quiz on your computer, players join from their phones by scanning a QR code or entering a PIN. Questions are displayed on the big screen, answers are tapped on phones, and a live leaderboard keeps score.

## Features

**Quiz management**
- Create, edit, and delete quizzes with a full web UI
- True/False, 4-choice, or 6-choice questions
- Per-question images and configurable time limits (5-120s)
- Per-quiz background image and background music
- Image uploads are auto-compressed (resized to 1920px max, JPEG at 80%)

**Real-time gameplay**
- 6-digit game PIN + QR code for instant mobile join
- Live question display on the host screen (projector/TV)
- Colored answer buttons on player phones
- Server-side timer auto-closes questions
- Time-based scoring: faster correct answers earn more points (500-1000)

**Leaderboard & animations**
- Animated score counting and staggered leaderboard reveals between rounds
- Speed stats: "Answered in 1.3s - faster than 80% of players!"
- Answer distribution bar chart after each question
- Dramatic top-3 podium reveal at game end with confetti

**Security**
- Argon2 password hashing
- JWT authentication (24-hour tokens)
- Proof-of-Work captcha on login/register (no external services)
- Uploaded media auto-cleaned when quizzes are deleted or updated

## Quick start

### Prerequisites

- [Docker](https://docs.docker.com/get-docker/) and Docker Compose
- [Rust](https://rustup.rs/) (for local development)

### Development

```bash
# Start the database
docker compose up -d db

# Run the server locally (with hot reload via cargo-watch, or plain cargo run)
DATABASE_URL="postgres://kahoot:kahoot@localhost:5433/kahoot" cargo run
```

The server prints:
```
INFO kahoot_free: listening on http://localhost:3000
INFO kahoot_free: network: http://192.168.1.42:3000
INFO kahoot_free: players join at: http://192.168.1.42:3000/player.html
```

Open `http://localhost:3000` on your computer (the host screen).
Players open the network URL on their phones.

### Production

```bash
docker compose -f docker-compose.prod.yml up -d
```

This builds a multi-stage Docker image (~93MB) with a compiled release binary. Uploads are stored on the host at `./data/uploads/`.

## How it works

### Architecture

```
Browser (host)  ──WebSocket──>  Axum server  <──WebSocket──  Browser (player)
     |                              |                              |
     |         REST API             |          REST API            |
     +------ /api/quizzes -------->+<------- /ws/play/{pin} ------+
     +------ /api/games   -------->|
     +------ /ws/host/{pin} ------>|
                                    |
                                PostgreSQL
                                (users, quizzes, questions, answers)
```

- **Host screen** (`/` - index.html): Quiz management, game control, projected display
- **Player screen** (`/player.html`): Mobile-first, join via PIN, tap answers
- **Game state**: In-memory (not in DB) for real-time performance. Quiz data is loaded from DB when a game starts; scores, timers, and player connections live in memory.

### Game flow

1. Host creates a quiz (REST API with JWT auth)
2. Host starts a game from a quiz -> server generates a 6-digit PIN
3. Host screen shows PIN + QR code, connects via WebSocket
4. Players scan QR / enter PIN on their phone, connect via WebSocket, send nickname
5. Host clicks "Start" -> server sends first question to all connected clients
6. Players tap an answer -> server records it with millisecond timing
7. When all answer (or timer expires) -> server scores, sends results to host + individual feedback to each player
8. Host clicks "Next" -> repeat until last question
9. Final screen: dramatic 3rd -> 2nd -> 1st reveal with confetti

### Scoring

Correct answers earn 500-1000 points based on speed:

```
points = 1000 - (500 * time_taken_ms / time_limit_ms)
```

Answer instantly = 1000 pts. Answer at the deadline = 500 pts. Wrong answer = 0 pts.

### Proof-of-Work captcha

Login and registration are protected by a self-hosted PoW challenge (no Google/Cloudflare dependency):

1. Client fetches `GET /api/auth/challenge` -> `{challenge, difficulty: 4}`
2. Client finds a nonce where `SHA-256(challenge + ":" + nonce)` starts with 4 hex zeros (~65k hashes, ~100-300ms in a browser)
3. Server verifies the challenge HMAC, timestamp freshness (120s), and hash difficulty

This makes mass bot registration expensive without adding friction for real users.

## API reference

### Authentication

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| GET | `/api/auth/challenge` | No | Get a PoW challenge |
| POST | `/api/auth/register` | No | Register (needs PoW) |
| POST | `/api/auth/login` | No | Login (needs PoW) |

### Quizzes

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| GET | `/api/quizzes` | JWT | List your quizzes |
| POST | `/api/quizzes` | JWT | Create a quiz |
| GET | `/api/quizzes/{id}` | JWT | Get quiz with questions/answers |
| PUT | `/api/quizzes/{id}` | JWT | Update a quiz |
| DELETE | `/api/quizzes/{id}` | JWT | Delete a quiz |

### Games

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| POST | `/api/games/{quiz_id}` | JWT | Create a game session (returns PIN) |
| GET | `/api/games/{pin}/qr?url=...` | No | Get QR code SVG |

### Other

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| POST | `/api/upload` | No | Upload a file (max 20MB) |
| GET | `/health` | No | Health check |

### WebSocket

| Endpoint | Role | Description |
|----------|------|-------------|
| `/ws/host/{pin}` | Host | Control game flow, receive player events |
| `/ws/play/{pin}` | Player | Join game, submit answers, receive questions |

## Configuration

| Env var | Default | Description |
|---------|---------|-------------|
| `DATABASE_URL` | `postgres://kahoot:kahoot@localhost:5433/kahoot` | PostgreSQL connection string |
| `PORT` | `3000` | Server listen port |

## Project structure

```
src/
  main.rs              # Server startup, routes, AppState
  auth.rs              # JWT token creation/verification
  models.rs            # DB structs + API request/response types
  game.rs              # In-memory game state, scoring, PIN generation
  pow.rs               # Proof-of-Work challenge/verification
  handlers/
    auth.rs            # Register, login, challenge endpoints
    quiz.rs            # Quiz CRUD
    game.rs            # Game creation, WebSocket handlers, game flow
    upload.rs          # File upload with image compression
migrations/            # SQL migrations (auto-run on startup)
static/
  index.html           # Host interface
  player.html          # Player interface (mobile-first)
docker-compose.yml     # Dev: PostgreSQL + cargo-watch
docker-compose.prod.yml # Prod: compiled binary + host-mounted uploads
Dockerfile             # Dev image
Dockerfile.prod        # Multi-stage production image
```

## Tech stack

| Layer | Choice | Why |
|-------|--------|-----|
| Web framework | Axum | Tokio-native, built-in WebSocket, tower middleware |
| Database | PostgreSQL + SQLx | Async, compile-time query checking, migration support |
| Auth | Argon2 + JWT | Gold standard password hashing + stateless tokens |
| Anti-bot | SHA-256 PoW | Self-hosted, no external services |
| Image processing | image crate | Resize + JPEG compression |
| QR codes | qrcode crate | Server-side SVG generation |

## License

MIT
