use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use commerce::link_access::{self, Scope, Source};
use commerce::reviews::{self, ReviewInvitation, ReviewText};
use commerce::withdrawals::{self, DeclareInput, LinkRequest, WithdrawalForm, WithdrawalReceipt};
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::customer::{header_str, ip_hash, no_store};
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
