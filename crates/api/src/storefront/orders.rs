//! The order page (spec §9.4, A4, A10): `checkout.<shop>/o/<token>` and its payment status
//! and retries. The order capability token (90 days) is a **read-only** credential: the page
//! works from the confirmation email in any browser. Paying (a new attempt, provider init)
//! additionally needs the checkout cart capability the order was placed with (the same
//! browser) or the session of the order's customer. The edge maps `/_p/orders/<token>/*` here
//! and forwards both. On a same-origin shop the page is `/o/<order id>` and the path carries
//! no credential: a link grant reads, the placing checkout capability pays (`order_access`).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use commerce::customers;
use commerce::order_access::{self, Access, Credentials};
use commerce::orders::{self, OrderView, PaymentView};
use commerce::payments::{self, Outcome, stripe};
use commerce::storefront::Context;
use platform::Error;
use platform::db::TenantTx;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::checkout::{PaymentStart, start_payment};
use super::customer::{SESSION_HEADER, header_str, no_store};
use super::link_grants::GRANT_HEADER;
use super::{CART_HEADER, Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::parse_json;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_order))
        .routes(routes!(get_payment))
        .routes(routes!(new_attempt))
        .routes(routes!(init_attempt))
        .routes(routes!(simulate_attempt))
}

/// What proves the right to pay (documentation only).
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
pub(crate) struct PayerHeaders {
    /// The checkout cart capability the order was placed with (`__Host-cart`).
    #[param(rename = "X-Cart-Token")]
    x_cart_token: Option<String>,
    /// The session of the order's customer (`__Host-sid`).
    #[param(rename = "X-Customer-Session")]
    x_customer_session: Option<String>,
    /// Same-origin shop: the link grant from the `__Host-Http-cap` cookie (reads only).
    #[param(rename = "X-Link-Grant")]
    x_link_grant: Option<String>,
}

/// An order path segment: the order id on a same-origin shop, else the order capability token.
pub(super) enum OrderRef {
    Id(Uuid),
    Token(String),
}

pub(super) fn order_ref(path: Result<Path<String>, PathRejection>) -> Result<OrderRef, Error> {
    let Path(segment) = path.map_err(|_| Error::NotFound)?;
    Ok(match Uuid::try_parse(&segment) {
        Ok(id) if segment.len() == 36 => OrderRef::Id(id),
        _ => OrderRef::Token(segment),
    })
}

/// Whether the requester may pay `order` (see the module docs).
pub(super) async fn may_pay(
    tx: &mut TenantTx,
    ctx: &Context,
    headers: &HeaderMap,
    order: Uuid,
) -> Result<bool, Error> {
    let customer = match header_str(headers, SESSION_HEADER) {
        Some(t) => customers::authenticate(tx, t).await?.map(|s| s.customer_id),
        None => None,
    };
    order_access::may_pay(tx, ctx, order, header_str(headers, CART_HEADER), customer).await
}

pub(super) struct Authorized {
    pub(super) order: Uuid,
    may_pay: bool,
    page: String,
}

/// The order `r` names and what this requester may do with it, `404` when nothing.
pub(super) async fn authorize(
    tx: &mut TenantTx,
    ctx: &Context,
    headers: &HeaderMap,
    r: &OrderRef,
) -> Result<Authorized, Error> {
    match r {
        OrderRef::Token(t) => {
            let order = orders::by_token(tx, t).await?;
            Ok(Authorized {
                order,
                may_pay: may_pay(tx, ctx, headers, order).await?,
                page: format!("/o/{t}"),
            })
        }
        OrderRef::Id(id) => {
            let credentials = Credentials {
                checkout: header_str(headers, CART_HEADER),
                grant: header_str(headers, GRANT_HEADER),
            };
            let access = order_access::authorize(tx, ctx, *id, credentials).await?;
            Ok(Authorized {
                order: *id,
                may_pay: access == Access::Pay,
                page: format!("/o/{id}"),
            })
        }
    }
}

async fn authorize_payer(
    tx: &mut TenantTx,
    ctx: &Context,
    headers: &HeaderMap,
    r: &OrderRef,
) -> Result<Authorized, Error> {
    let a = authorize(tx, ctx, headers, r).await?;
    if a.may_pay { Ok(a) } else { Err(not_allowed()) }
}

/// The order, its payment view narrowed to what this requester may do.
async fn read(
    tx: &mut TenantTx,
    ctx: &Context,
    headers: &HeaderMap,
    r: &OrderRef,
) -> Result<OrderView, Error> {
    let a = authorize(tx, ctx, headers, r).await?;
    let mut view = orders::view(tx, a.order).await?;
    view.payment.can_pay = a.may_pay;
    view.payment.can_retry &= a.may_pay;
    Ok(view)
}

#[utoipa::path(
    get,
    path = "/storefront/v1/orders/{token}",
    tag = "storefront",
    params(StorefrontHeaders, PayerHeaders, ("token" = String, Path, description = "Order capability token, or the order id on a same-origin shop")),
    responses(
        (status = 200, body = OrderView),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_order(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    token_path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let r = order_ref(token_path)?;
    let view = with_ctx(&s, &shopper, async |tx, ctx| {
        read(tx, ctx, &headers, &r).await
    })
    .await?;
    Ok(no_store(Json(view).into_response()))
}

/// The payment status (A10), polled by the order page while a payment is under way.
#[utoipa::path(
    get,
    path = "/storefront/v1/orders/{token}/payment",
    tag = "storefront",
    params(StorefrontHeaders, PayerHeaders, ("token" = String, Path, description = "Order capability token, or the order id on a same-origin shop")),
    responses(
        (status = 200, body = PaymentView),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_payment(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    token_path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let r = order_ref(token_path)?;
    let view = with_ctx(&s, &shopper, async |tx, ctx| {
        Ok(read(tx, ctx, &headers, &r).await?.payment)
    })
    .await?;
    Ok(no_store(Json(view).into_response()))
}

fn not_allowed() -> Error {
    Error::Forbidden {
        code: "payment_not_allowed",
    }
}

/// A new payment attempt after a failed one, within the order's payment window (A10). Needs
/// the right to pay (`403 payment_not_allowed`).
#[utoipa::path(
    post,
    path = "/storefront/v1/orders/{token}/payment-attempts",
    tag = "storefront",
    params(StorefrontHeaders, PayerHeaders, ("token" = String, Path, description = "Order capability token, or the order id on a same-origin shop")),
    responses(
        (status = 201, body = PaymentStart),
        (status = 403, description = "payment_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "retry_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn new_attempt(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    token_path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let r = order_ref(token_path)?;
    let (attempt, page) = with_ctx(&s, &shopper, async |tx, ctx| {
        let a = authorize_payer(tx, ctx, &headers, &r).await?;
        Ok((
            payments::retry(tx, &s.checkout.payments, a.order).await?,
            a.page,
        ))
    })
    .await?;
    let start = start_payment(&s, shopper.tenant_id, attempt, &page).await;
    Ok(no_store((StatusCode::CREATED, Json(start)).into_response()))
}

/// Initializes (again) a pending attempt at its provider (A10): idempotent, for when the
/// automatic init after placement failed or the customer comes back to pay. Needs the right
/// to pay; `409 payment_window_closed` after the order's payment deadline.
#[utoipa::path(
    post,
    path = "/storefront/v1/orders/{token}/payment-attempts/{attempt}/init",
    tag = "storefront",
    params(
        StorefrontHeaders,
        PayerHeaders,
        ("token" = String, Path, description = "Order capability token, or the order id on a same-origin shop"),
        ("attempt" = Uuid, Path),
    ),
    responses(
        (status = 200, body = PaymentStart),
        (status = 403, description = "payment_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "attempt_not_pending | payment_window_closed", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn init_attempt(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(String, Uuid)>, PathRejection>,
) -> Result<Response, Error> {
    let Path((segment, attempt)) = path.map_err(|_| Error::NotFound)?;
    let r = order_ref(Ok(Path(segment)))?;
    let page = with_ctx(&s, &shopper, async |tx, ctx| {
        let a = authorize(tx, ctx, &headers, &r).await?;
        if payments::attempt(tx, attempt).await?.order_id != a.order {
            return Err(Error::NotFound);
        }
        if !a.may_pay {
            return Err(not_allowed());
        }
        Ok(a.page)
    })
    .await?;
    let action = payments::init(
        &s.db,
        shopper.tenant_id,
        &s.checkout.payments,
        attempt,
        &page,
    )
    .await?;
    Ok(no_store(
        Json(PaymentStart {
            attempt_id: attempt,
            action: Some(action),
        })
        .into_response(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SimulateInput {
    pub outcome: Outcome,
}

/// The "Stripe test simulator" (local mode without a real Stripe key; `404` otherwise): makes
/// the API sign a Stripe-shaped `payment_intent.succeeded` / `payment_intent.payment_failed`
/// event with the webhook secret and receive it like Stripe's, so verification, storage and
/// asynchronous processing run for real. Needs the right to pay. Answers `202` with the
/// payment as it is now; the order page polls until the event is processed.
#[utoipa::path(
    post,
    path = "/storefront/v1/orders/{token}/payment-attempts/{attempt}/simulate",
    tag = "storefront",
    params(
        StorefrontHeaders,
        PayerHeaders,
        ("token" = String, Path, description = "Order capability token, or the order id on a same-origin shop"),
        ("attempt" = Uuid, Path),
    ),
    request_body = SimulateInput,
    responses(
        (status = 202, body = PaymentView),
        (status = 403, description = "payment_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "attempt_not_initialized", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn simulate_attempt(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(String, Uuid)>, PathRejection>,
    body: Bytes,
) -> Result<Response, Error> {
    let Path((segment, attempt)) = path.map_err(|_| Error::NotFound)?;
    let r = order_ref(Ok(Path(segment)))?;
    let input: SimulateInput = parse_json(&body)?;
    let stripe = s
        .checkout
        .payments
        .stripe
        .as_ref()
        .filter(|s| s.simulator())
        .ok_or(Error::NotFound)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        let order = authorize(tx, ctx, &headers, &r).await?;
        let a = payments::attempt(tx, attempt).await?;
        if a.order_id != order.order || a.method != payments::MethodKind::Stripe {
            return Err(Error::NotFound);
        }
        if !order.may_pay {
            return Err(not_allowed());
        }
        Ok(())
    })
    .await?;
    stripe::simulate_payment(&s.db, stripe, shopper.tenant_id, attempt, input.outcome).await?;
    let view = with_ctx(&s, &shopper, async |tx, ctx| {
        Ok(read(tx, ctx, &headers, &r).await?.payment)
    })
    .await?;
    Ok(no_store((StatusCode::ACCEPTED, Json(view)).into_response()))
}
