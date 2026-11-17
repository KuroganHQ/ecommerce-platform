//! Link grants (decision C5) for same-origin shops. Only the edge calls these: it exchanges an
//! emailed link from an admitted navigation for a grant, keeps the grant in the `HttpOnly`
//! `__Host-Http-cap` cookie and forwards it as `X-Link-Grant`. The grant never appears in a
//! response body.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use commerce::link_access::{self, Link, Scope};
use commerce::orders::{self, OrderView};
use commerce::withdrawals::{self, LinkRequest};
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::customer::{header_str, no_store};
use super::{Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::parse_json;

const GRANT_HEADER: &str = "x-link-grant";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(exchange))
        .routes(routes!(order))
        .routes(routes!(request_withdrawal))
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
    let link = match &input.link {
        LinkInput::Order { token } => Link::Order(token),
        LinkInput::WithdrawalRequest => Link::WithdrawalRequest,
    };
    let granted = with_ctx(&s, &shopper, async |tx, ctx| {
        link_access::exchange(tx, ctx, link, input.previous.as_deref()).await
    })
    .await?;
    let resource = match granted.scope {
        Scope::OrderRead(id) => GrantedResource::Order { id },
        Scope::WithdrawalRequest => GrantedResource::WithdrawalRequest,
    };
    let mut res = (StatusCode::CREATED, Json(Exchanged { resource })).into_response();
    res.headers_mut().insert(
        GRANT_HEADER,
        HeaderValue::from_str(&granted.token).map_err(|e| Error::Internal(e.to_string()))?,
    );
    Ok(no_store(res))
}

/// The order a grant may read. Paying stays with the checkout capability or the customer.
#[utoipa::path(
    get,
    path = "/storefront/v1/link-grants/orders/{id}",
    tag = "storefront",
    params(StorefrontHeaders, GrantHeader, ("id" = Uuid, Path)),
    responses(
        (status = 200, body = OrderView),
        (status = 404, description = "no grant for this order", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn order(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, Error> {
    let g = grant(&headers)?;
    let mut view = with_ctx(&s, &shopper, async |tx, ctx| {
        link_access::authorize(tx, ctx, g, Scope::OrderRead(id)).await?;
        orders::view(tx, id).await
    })
    .await?;
    view.payment.can_pay = false;
    view.payment.can_retry = false;
    Ok(no_store(Json(view).into_response()))
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
