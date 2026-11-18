//! The store's subscription (SHOP-45), owners only. Checkout and the portal are Stripe-hosted
//! pages; the plan itself only changes through the billing webhook.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use commerce::billing::{self, BillingOverview};
use commerce::tenancy::Role;
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::admin::{TenantHeader, parse_json};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(overview))
        .routes(routes!(checkout))
        .routes(routes!(portal))
}

fn admin_origin(s: &AppState) -> Result<&str, Error> {
    s.admin_origin
        .to_str()
        .map_err(|_| Error::Internal("invalid admin origin".into()))
}

/// The store's plan and subscription.
#[utoipa::path(
    get,
    path = "/admin/v1/billing",
    tag = "billing",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 200, body = BillingOverview),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn overview(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<BillingOverview>, Error> {
    staff.require(Role::Owner)?;
    Ok(Json(
        billing::overview(&s.db, s.billing.as_ref(), staff.tenant_id).await?,
    ))
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CheckoutRequest {
    /// One of the configured plans.
    pub plan: String,
}

#[derive(Serialize, ToSchema)]
pub struct StripeRedirect {
    /// The Stripe-hosted page to send the owner to.
    pub url: String,
}

/// A Stripe Checkout page subscribing the store to `plan` (login at most 15 minutes old).
#[utoipa::path(
    post,
    path = "/admin/v1/billing/checkout",
    tag = "billing",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = CheckoutRequest,
    responses(
        (status = 200, body = StripeRedirect),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "subscription_exists", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "unknown_plan", body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, description = "billing_not_configured", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn checkout(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<StripeRedirect>, Error> {
    staff.require(Role::Owner)?;
    staff.require_fresh_auth()?;
    let input: CheckoutRequest = parse_json(&body)?;
    let url = billing::checkout(
        &s.db,
        s.billing.as_ref(),
        staff.tenant_id,
        &staff.user.email,
        &input.plan,
        admin_origin(&s)?,
    )
    .await?;
    Ok(Json(StripeRedirect { url }))
}

/// The Stripe customer portal: card, invoices, plan changes and cancellation (login at most
/// 15 minutes old).
#[utoipa::path(
    post,
    path = "/admin/v1/billing/portal",
    tag = "billing",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 200, body = StripeRedirect),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "no_billing_account", body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, description = "billing_not_configured", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn portal(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<StripeRedirect>, Error> {
    staff.require(Role::Owner)?;
    staff.require_fresh_auth()?;
    let url = billing::portal(
        &s.db,
        s.billing.as_ref(),
        staff.tenant_id,
        admin_origin(&s)?,
    )
    .await?;
    Ok(Json(StripeRedirect { url }))
}
