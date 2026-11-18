//! Merchant self-signup (SHOP-45), on only with `SELF_SIGNUP=1`. `POST /signup` proves the email
//! with a sign-in link, answering alike for new and known addresses; a verified user without a
//! store then creates one with `POST /admin/v1/stores`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use commerce::staff;
use commerce::tenancy::{self, NewTenant};
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::parse_json;
use crate::auth::StaffUser;
use crate::storefront::customer::CLIENT_IP_HEADER;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(signup_status, signup))
        .routes(routes!(create_store))
}

#[derive(Serialize, ToSchema)]
pub struct SignupStatus {
    pub enabled: bool,
}

/// Whether self-signup is on. `404` when it is off.
#[utoipa::path(
    get,
    path = "/signup",
    tag = "signup",
    responses(
        (status = 200, body = SignupStatus),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn signup_status(State(s): State<AppState>) -> Result<Json<SignupStatus>, Error> {
    s.self_signup.as_ref().ok_or(Error::NotFound)?;
    Ok(Json(SignupStatus { enabled: true }))
}

#[derive(Deserialize, ToSchema)]
pub struct SignupRequest {
    pub email: String,
}

#[derive(Serialize, ToSchema)]
pub struct SignupAccepted {
    /// Always `true`: whether the address already had an account is not revealed.
    pub accepted: bool,
}

/// Mails a sign-in link to the address, creating the (unverified) user when new. The answer is
/// the same for every valid address. Rate-limited per client IP (`429 rate_limited`).
#[utoipa::path(
    post,
    path = "/signup",
    tag = "signup",
    request_body = SignupRequest,
    responses(
        (status = 202, body = SignupAccepted),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_email", body = platform::Problem, content_type = "application/problem+json"),
        (status = 429, description = "rate_limited", body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn signup(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let limiter = s.self_signup.as_ref().ok_or(Error::NotFound)?;
    let ip = headers
        .get(CLIENT_IP_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    if let Err(wait) = limiter.check("signup", ip) {
        return Ok(crate::rate_limit::rate_limited(wait));
    }
    let input: SignupRequest = parse_json(&body)?;
    let email = staff::normalize_email(&input.email)?;
    let auth = s.auth_service.as_ref().ok_or_else(|| {
        Error::Unavailable("self-signup needs the auth service (AUTH_INTERNAL_URL)".into())
    })?;
    let callback = s
        .admin_origin
        .to_str()
        .map_err(|_| Error::Internal("invalid admin origin".into()))?;
    auth.ensure_user(&email, &email).await?;
    auth.invite(&email, callback).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(SignupAccepted { accepted: true }),
    )
        .into_response())
}

#[derive(Deserialize, ToSchema)]
pub struct NewStore {
    pub name: String,
    /// The store's subdomain: 1-63 lowercase letters, digits or single inner hyphens.
    pub slug: String,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedStore {
    /// Send as `X-Tenant-Id` to act in the new store.
    pub tenant_id: Uuid,
    pub hostname: String,
}

/// Creates a store owned by the caller, like `api admin create-tenant`. One per user
/// (`409 store_limit_reached`); platform superadmins are not limited. `404` unless
/// `SELF_SIGNUP=1`.
#[utoipa::path(
    post,
    path = "/admin/v1/stores",
    tag = "signup",
    security(("staff_jwt" = [])),
    request_body = NewStore,
    responses(
        (status = 201, body = CreatedStore),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "store_limit_reached | already_exists", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_store(
    user: StaffUser,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<(StatusCode, Json<CreatedStore>), Error> {
    s.self_signup.as_ref().ok_or(Error::NotFound)?;
    let input: NewStore = parse_json(&body)?;
    let superadmin = crate::admin_platform::is_superadmin(&s.db, &user.user_id).await?;
    let created = tenancy::create_tenant(
        &s.db,
        &NewTenant {
            slug: input.slug.trim(),
            name: &input.name,
            owner_user_id: &user.user_id,
            owner_email: &user.email,
            signup_user_id: (!superadmin).then_some(user.user_id.as_str()),
        },
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedStore {
            tenant_id: created.tenant_id,
            hostname: created.hostname,
        }),
    ))
}
