use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use std::sync::LazyLock;
use uuid::Uuid;

use crate::models::Claims;
use crate::AppState;

/// Signing secret for JWTs. Read once from the `JWT_SECRET` env var; panics if
/// unset (validated at startup in main).
static JWT_SECRET: LazyLock<String> =
    LazyLock::new(|| std::env::var("JWT_SECRET").expect("JWT_SECRET must be set"));

pub fn create_token(user_id: Uuid, is_admin: bool) -> Result<String, StatusCode> {
    let exp = (chrono::Utc::now() + chrono::Duration::hours(24)).timestamp() as usize;
    let claims = Claims {
        sub: user_id,
        exp,
        admin: is_admin,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub fn verify_token(token: &str) -> Result<Claims, StatusCode> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(JWT_SECRET.as_bytes()),
        &Validation::default(),
    )
    .map(|data| data.claims)
    .map_err(|_| StatusCode::UNAUTHORIZED)
}

fn extract_claims(parts: &Parts) -> Result<Claims, StatusCode> {
    let header = parts
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let token = header
        .strip_prefix("Bearer ")
        .ok_or(StatusCode::UNAUTHORIZED)?;
    verify_token(token)
}

/// Extractor: any authenticated user. Carries (user_id, is_admin).
pub struct AuthUser(pub Uuid, pub bool);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let claims = extract_claims(parts)?;
        Ok(AuthUser(claims.sub, claims.admin))
    }
}

/// Extractor: authenticated user that is an admin. Returns 403 if not admin.
pub struct AdminUser(pub Uuid);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let claims = extract_claims(parts)?;
        if !claims.admin {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(AdminUser(claims.sub))
    }
}
