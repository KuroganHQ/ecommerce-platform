use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use commerce::flows;
use commerce::link_access::{self, Scope, Source};
use commerce::marketing::campaigns::{self, Preferences};
use commerce::marketing::subscribers::{self, Confirmation};
use commerce::reviews::{self, ReviewInvitation, ReviewText};
use commerce::storefront::Context;
use commerce::withdrawals::{self, DeclareInput, LinkRequest, WithdrawalForm, WithdrawalReceipt};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::customer::{header_str, ip_hash, no_store};
use super::flows::RestoreResult;
use super::newsletter::NewsletterStatus;
use super::reviews::ReviewSubmitted;
use super::{Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::parse_json;

pub(super) const GRANT_HEADER: &str = "x-link-grant";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(exchange))
        .routes(routes!(request_withdrawal))
        .routes(routes!(withdrawal_form, declare_withdrawal))
        .routes(routes!(review_invitation, submit_review))
        .routes(routes!(newsletter_confirmation, confirm_newsletter))
        .routes(routes!(newsletter_preferences))
        .routes(routes!(newsletter_unsubscribe))
        .routes(routes!(newsletter_resubscribe))
        .routes(routes!(confirm_watch))
        .routes(routes!(unsubscribe_watch))
        .routes(routes!(unsubscribe_flow))
        .routes(routes!(restore_cart_by_grant))
}

#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
pub(crate) struct GrantHeader {
    /// The link grant from the `__Host-Http-cap` cookie.
    #[param(rename = "X-Link-Grant")]
    x_link_grant: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LinkInput {
    /// The order email link (`/o/<token>`); it stays valid until its own expiry.
    Order { token: String },
    /// The public withdrawal form, which has no link.
    WithdrawalRequest,
    /// The withdrawal email link (`/withdraw?t=`); single use, consumed by the declaration.
    Withdrawal { token: String },
    /// The review email link (`/review?token=`); single use, consumed by the review.
    Review { token: String },
    /// The double opt-in link (`/newsletter/confirm?token=`); consumed by the confirmation.
    NewsletterConfirmation { token: String },
    /// A campaign's preferences link (`/newsletter?t=`); reusable.
    NewsletterPreferences { token: String },
    /// A stock or price alert's double opt-in link (`/watch/confirm?token=`).
    WatchConfirmation { token: String },
    /// A fired alert's cancel link (`/watch/unsubscribe?token=`).
    WatchUnsubscribe { token: String },
    /// An abandoned-cart reminder's opt-out link (`/flows/unsubscribe?token=`).
    FlowUnsubscribe { token: String },
    /// An abandoned-cart reminder's restore link (`/restore-cart?token=`); single use.
    RestoreCart { token: String },
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExchangeInput {
    link: LinkInput,
    /// The grant the browser held before; it is revoked.
    previous: Option<String>,
}

#[derive(Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantedResource {
    Order { id: Uuid },
    WithdrawalRequest,
    Withdrawal,
    Review,
    NewsletterConfirmation,
    NewsletterPreferences,
    WatchConfirmation,
    WatchUnsubscribe,
    FlowUnsubscribe,
    RestoreCart,
}

#[derive(Serialize, ToSchema)]
pub struct Exchanged {
    resource: GrantedResource,
}

fn grant(headers: &HeaderMap) -> Result<&str, Error> {
    header_str(headers, GRANT_HEADER).ok_or(Error::NotFound)
}

/// Exchanges a link for a grant scoped to its resource and action, this origin and its
/// checkout mode, and revokes `previous`. The grant is in the `X-Link-Grant` response header.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants",
    tag = "storefront",
    params(StorefrontHeaders),
    request_body = ExchangeInput,
    responses(
        (status = 201, body = Exchanged, headers(("X-Link-Grant" = String, description = "The new grant"))),
        (status = 404, description = "unknown or expired link", body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "checkout_mode_changed", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn exchange(
    shopper: Shopper,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    let input: ExchangeInput = parse_json(&body)?;
    let source = match &input.link {
        LinkInput::Order { token } => Source::OrderLink(token),
        LinkInput::WithdrawalRequest => Source::WithdrawalForm,
        LinkInput::Withdrawal { token } => Source::WithdrawalLink(token),
        LinkInput::Review { token } => Source::ReviewLink(token),
        LinkInput::NewsletterConfirmation { token } => {
            Source::MarketingLink(Scope::NewsletterConfirmation, token)
        }
        LinkInput::NewsletterPreferences { token } => {
            Source::MarketingLink(Scope::NewsletterPreferences, token)
        }
        LinkInput::WatchConfirmation { token } => {
            Source::MarketingLink(Scope::WatchConfirmation, token)
        }
        LinkInput::WatchUnsubscribe { token } => {
            Source::MarketingLink(Scope::WatchUnsubscribe, token)
        }
        LinkInput::FlowUnsubscribe { token } => {
            Source::MarketingLink(Scope::FlowUnsubscribe, token)
        }
        LinkInput::RestoreCart { token } => Source::MarketingLink(Scope::RestoreCart, token),
    };
    let granted = with_ctx(&s, &shopper, async |tx, ctx| {
        link_access::exchange(tx, ctx, source, input.previous.as_deref()).await
    })
    .await?;
    let resource = match granted.scope {
        Scope::OrderRead(id) => GrantedResource::Order { id },
        Scope::WithdrawalRequest => GrantedResource::WithdrawalRequest,
        Scope::Withdrawal => GrantedResource::Withdrawal,
        Scope::Review => GrantedResource::Review,
        Scope::NewsletterConfirmation => GrantedResource::NewsletterConfirmation,
        Scope::NewsletterPreferences => GrantedResource::NewsletterPreferences,
        Scope::WatchConfirmation => GrantedResource::WatchConfirmation,
        Scope::WatchUnsubscribe => GrantedResource::WatchUnsubscribe,
        Scope::FlowUnsubscribe => GrantedResource::FlowUnsubscribe,
        Scope::RestoreCart => GrantedResource::RestoreCart,
    };
    let mut res = (StatusCode::CREATED, Json(Exchanged { resource })).into_response();
    res.headers_mut().insert(
        GRANT_HEADER,
        HeaderValue::from_str(&granted.token).map_err(|e| Error::Internal(e.to_string()))?,
    );
    Ok(no_store(res))
}

/// The public withdrawal form's request (as `POST /withdrawals`, always `202`). It completes
/// the anonymous grant in the same transaction, so a refused request keeps it.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants/withdrawal-request",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    request_body = LinkRequest,
    responses(
        (status = 202),
        (status = 404, description = "no withdrawal-request grant", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_email", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn request_withdrawal(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, Error> {
    let g = grant(&headers)?;
    let input: LinkRequest = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        link_access::complete(tx, ctx, g, Scope::WithdrawalRequest).await?;
        withdrawals::request_link(tx, ctx, &input).await
    })
    .await?;
    Ok(StatusCode::ACCEPTED)
}

/// The withdrawal form of the grant's emailed link. Reading consumes nothing.
#[utoipa::path(
    get,
    path = "/storefront/v1/link-grants/withdrawal",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses(
        (status = 200, body = WithdrawalForm),
        (status = 404, description = "no withdrawal grant, or its link was used or expired", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn withdrawal_form(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let g = grant(&headers)?;
    let form = with_ctx(&s, &shopper, async |tx, ctx| {
        withdrawals::form_by_grant(tx, ctx, g).await
    })
    .await?;
    Ok(no_store(Json(form).into_response()))
}

/// The explicit confirmation (A19) on a same-origin shop. It consumes the emailed link and
/// revokes the grant in the same transaction, so a refused declaration keeps both.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants/withdrawal",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    request_body = DeclareInput,
    responses(
        (status = 201, body = WithdrawalReceipt),
        (status = 404, description = "no withdrawal grant, or its link was used or expired", body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "not_withdrawable", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "confirmation_required | invalid_withdrawal | iban_required | invalid_iban", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn declare_withdrawal(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<WithdrawalReceipt>), Error> {
    let g = grant(&headers)?;
    let input: DeclareInput = parse_json(&body)?;
    let receipt = with_ctx(&s, &shopper, async |tx, ctx| {
        withdrawals::declare_by_grant(tx, ctx, g, &input).await
    })
    .await?;
    Ok((StatusCode::CREATED, Json(receipt)))
}

/// The order line of the grant's review link. Reading consumes nothing.
#[utoipa::path(
    get,
    path = "/storefront/v1/link-grants/review",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses(
        (status = 200, body = ReviewInvitation),
        (status = 404, description = "no review grant, or its link was used or expired", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn review_invitation(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let g = grant(&headers)?;
    let invitation = with_ctx(&s, &shopper, async |tx, ctx| {
        reviews::invitation_by_grant(tx, ctx, g).await
    })
    .await?;
    Ok(no_store(Json(invitation).into_response()))
}

/// Submits the review of the grant's order line. It consumes the emailed link and revokes the
/// grant in the same transaction, so a refused review keeps both.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants/review",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    request_body = ReviewText,
    responses(
        (status = 201, body = ReviewSubmitted),
        (status = 404, description = "no review grant, or its link was used or expired", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
        (status = 429, description = "too_many_reviews", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn submit_review(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let g = grant(&headers)?;
    let input: ReviewText = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        let ip = ip_hash(tx, &headers).await?;
        reviews::submit_by_grant(tx, ctx, g, &input, ip.as_deref()).await
    })
    .await?;
    Ok(no_store(
        (StatusCode::CREATED, Json(ReviewSubmitted::pending())).into_response(),
    ))
}

/// The masked address of the grant's pending sign-up. Reading consumes nothing.
#[utoipa::path(
    get,
    path = "/storefront/v1/link-grants/newsletter-confirmation",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses(
        (status = 200, body = Confirmation),
        (status = 404, description = "no confirmation grant, or its link was used or expired", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn newsletter_confirmation(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let g = grant(&headers)?;
    let c = with_ctx(&s, &shopper, async |tx, ctx| {
        subscribers::confirmation_by_grant(tx, ctx, g).await
    })
    .await?;
    Ok(no_store(Json(c).into_response()))
}

/// Confirms the grant's sign-up and revokes the grant in the same transaction.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants/newsletter-confirmation",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses(
        (status = 200, body = NewsletterStatus),
        (status = 404, description = "no confirmation grant, or its link was used or expired", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn confirm_newsletter(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let g = grant(&headers)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        let ip = ip_hash(tx, &headers).await?;
        subscribers::confirm_by_grant(tx, ctx, g, ip.as_deref()).await
    })
    .await?;
    Ok(no_store(
        Json(NewsletterStatus {
            status: "subscribed".into(),
        })
        .into_response(),
    ))
}

/// The subscription behind the grant's campaign email.
#[utoipa::path(
    get,
    path = "/storefront/v1/link-grants/newsletter-preferences",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses(
        (status = 200, body = Preferences),
        (status = 404, description = "no preferences grant", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn newsletter_preferences(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let g = grant(&headers)?;
    let p = with_ctx(&s, &shopper, async |tx, ctx| {
        campaigns::preferences_by_grant(tx, ctx, g).await
    })
    .await?;
    Ok(no_store(Json(p).into_response()))
}

/// The preferences page's unsubscribe button. The grant stays for subscribing again.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants/newsletter-preferences/unsubscribe",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses(
        (status = 200, body = Preferences),
        (status = 404, description = "no preferences grant", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn newsletter_unsubscribe(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let g = grant(&headers)?;
    let p = with_ctx(&s, &shopper, async |tx, ctx| {
        let ip = ip_hash(tx, &headers).await?;
        campaigns::unsubscribe_by_grant(tx, ctx, g, ip.as_deref()).await
    })
    .await?;
    Ok(no_store(Json(p).into_response()))
}

/// The preferences page's subscribe-again button: a new double opt-in mail.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants/newsletter-preferences/resubscribe",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses(
        (status = 202, body = NewsletterStatus),
        (status = 404, description = "no preferences grant", body = platform::Problem, content_type = "application/problem+json"),
        (status = 429, description = "too_many_signups", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn newsletter_resubscribe(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let g = grant(&headers)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        let ip = ip_hash(tx, &headers).await?;
        campaigns::resubscribe_by_grant(tx, ctx, g, ip.as_deref()).await
    })
    .await?;
    Ok(no_store(
        (
            StatusCode::ACCEPTED,
            Json(NewsletterStatus {
                status: "accepted".into(),
            }),
        )
            .into_response(),
    ))
}

async fn grant_action(
    s: &AppState,
    shopper: &Shopper,
    headers: &HeaderMap,
    action: impl AsyncFnOnce(&mut TenantTx, &Context, &str) -> Result<(), Error>,
) -> Result<StatusCode, Error> {
    let g = grant(headers)?;
    with_ctx(s, shopper, async |tx, ctx| action(tx, ctx, g).await).await?;
    Ok(StatusCode::OK)
}

/// Confirms the grant's stock or price alert and revokes the grant.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants/watch-confirmation",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses((status = 200), (status = 404, body = platform::Problem, content_type = "application/problem+json"))
)]
async fn confirm_watch(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, Error> {
    grant_action(&s, &shopper, &headers, flows::confirm_watch_by_grant).await
}

/// Cancels the grant's stock or price alert and revokes the grant.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants/watch-unsubscribe",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses((status = 200), (status = 404, body = platform::Problem, content_type = "application/problem+json"))
)]
async fn unsubscribe_watch(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, Error> {
    grant_action(&s, &shopper, &headers, flows::unsubscribe_watch_by_grant).await
}

/// Stops abandoned-cart reminders for the grant's address and revokes the grant.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants/flow-unsubscribe",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses((status = 200), (status = 404, body = platform::Problem, content_type = "application/problem+json"))
)]
async fn unsubscribe_flow(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, Error> {
    grant_action(&s, &shopper, &headers, flows::unsubscribe_cart_by_grant).await
}

/// Restores the grant's abandoned cart as a new shop cart (lines and coupons only), consumes
/// the link and revokes the grant. The shop capability is for the edge's `cart` cookie.
#[utoipa::path(
    post,
    path = "/storefront/v1/link-grants/restore-cart",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader),
    responses(
        (status = 200, body = RestoreResult),
        (status = 404, description = "no restore grant, or its link was used or expired", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn restore_cart_by_grant(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let g = grant(&headers)?;
    let cart_token = with_ctx(&s, &shopper, async |tx, ctx| {
        flows::restore_by_grant(tx, ctx, g).await
    })
    .await?;
    Ok(no_store(Json(RestoreResult { cart_token }).into_response()))
}
