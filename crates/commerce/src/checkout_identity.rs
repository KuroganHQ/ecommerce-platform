//! Sign-in from a same-origin checkout, checkout half (decision A2). The shop never holds an
//! account session: identify (steps 1-3) only links the checkout to the account the account
//! origin vouched for, as a prefill hint for its saved addresses. It never makes the checkout
//! visible to that account and never writes to it. Only a placement the account origin
//! confirms (step 4) attributes the order to that account.

use platform::Error;
use platform::db::TenantTx;
use uuid::Uuid;

use crate::account_codes::{self, Purpose};
use crate::capability;
use crate::checkout::{self, PlaceOrderInput, Placement, Placer, Settings};
use crate::checkout_access::{self, bind, enter, open_same_origin};
use crate::customers::{self, Address};
use crate::idempotency;
use crate::storefront::Context;
use crate::tenancy::CheckoutMode;

pub const STATE_TTL_SECS: f64 = 300.0;
pub const PLACEMENT_TTL_SECS: f64 = 300.0;

/// A2.1: a fresh single-use `state` for the open checkout behind `token`, valid 5 minutes and
/// bound to that capability, the exact origin and its generation. The browser keeps it as the
/// `__Host-Http-idt` cookie as well. Revokes the checkout's earlier unconsumed states.
pub async fn start(tx: &mut TenantTx, ctx: &Context, token: &str) -> Result<String, Error> {
    let (c, p) = open_same_origin(tx, ctx, token).await?;
    sqlx::query!(
        "DELETE FROM checkout_identities
         WHERE consumed_at IS NULL AND (cart_id = $1 OR expires_at < now())",
        c.id
    )
    .execute(&mut **tx)
    .await?;
    let state = capability::mint();
    sqlx::query!(
        "INSERT INTO checkout_identities
             (state_hash, tenant_id, cart_id, capability_hash, origin, generation, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, now() + make_interval(secs => $7))",
        state.hash,
        tx.tenant_id(),
        c.id,
        capability::hash(token),
        p.origin,
        p.generation,
        STATE_TTL_SECS
    )
    .execute(&mut **tx)
    .await?;
    Ok(state.token)
}

/// A2.3: accepts the account origin's callback only when `state` equals the browser's `idt`
/// cookie, is unused, unexpired and bound to the presented capability, origin and generation,
/// and `code` is an unused identify code issued for that `state` and origin. Consumes both,
/// links the checkout to the code's account and returns the replacing capability (C6), all in
/// the caller's transaction. Anything else is `404`.
pub async fn callback(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
    state: &str,
    idt: &str,
    code: &str,
) -> Result<String, Error> {
    if state != idt || !capability::well_formed(state) {
        return Err(Error::NotFound);
    }
    let (c, p) = open_same_origin(tx, ctx, token).await?;
    let origin = ctx.origin.as_ref().ok_or(Error::NotFound)?;
    let customer = account_codes::consume(tx, Purpose::Identify, state, origin, code)
        .await?
        .and_then(|e| e.customer_id)
        .ok_or(Error::NotFound)?;
    sqlx::query_scalar!(
        r#"UPDATE checkout_identities SET consumed_at = now(), customer_id = $6
           WHERE state_hash = $1 AND cart_id = $2 AND capability_hash = $3
             AND origin = $4 AND generation = $5
             AND consumed_at IS NULL AND expires_at > now()
           RETURNING 1 AS "one!""#,
        capability::hash(state),
        c.id,
        capability::hash(token),
        p.origin,
        p.generation,
        customer
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    bind(tx, c.id, &p).await?.ok_or(Error::NotFound)
}

/// The saved addresses of the account the checkout behind `token` was last identified with;
/// none before identify.
pub async fn saved_addresses(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
) -> Result<Vec<Address>, Error> {
    let (c, _) = open_same_origin(tx, ctx, token).await?;
    match linked_account(tx, c.id).await? {
        Some(id) => customers::addresses(tx, id).await,
        None => Ok(vec![]),
    }
}

async fn linked_account(tx: &mut TenantTx, cart_id: Uuid) -> Result<Option<Uuid>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT customer_id FROM checkout_identities
         WHERE cart_id = $1 AND consumed_at IS NOT NULL
         ORDER BY consumed_at DESC LIMIT 1",
        cart_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .flatten())
}

#[derive(Debug)]
pub enum Placing {
    Placed(Placement),
    /// A2.4: the open same-origin checkout is linked to an account, so placing it needs the
    /// account origin's confirmation first. Carries the placement `state` for that round trip.
    Confirm(String),
}

/// Places the order as [`checkout::place_order`] does, except for an open same-origin checkout
/// that identify linked to an account. That one gets a fresh single-use placement `state`
/// instead, valid 5 minutes and bound to the checkout's capability, exact origin and its
/// generation (which every mode switch moves), the linked (expected) account and the reviewed
/// `input`, notes and cart version included. The order waits for [`confirm_placement`].
/// Revokes the checkout's earlier unconsumed placement states.
#[allow(clippy::too_many_arguments)]
pub async fn place(
    tx: &mut TenantTx,
    ctx: &Context,
    settings: &Settings,
    token: &str,
    key: &str,
    request_hash: &str,
    input: &PlaceOrderInput,
    placer: &Placer<'_>,
) -> Result<Placing, Error> {
    if let Some(state) = placement_state(tx, ctx, token, input).await? {
        return Ok(Placing::Confirm(state));
    }
    checkout::place_order(tx, ctx, settings, token, key, request_hash, input, placer)
        .await
        .map(Placing::Placed)
}

async fn placement_state(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
    input: &PlaceOrderInput,
) -> Result<Option<String>, Error> {
    let p = enter(tx, ctx).await?;
    if p.mode != CheckoutMode::SameOrigin {
        return Ok(None);
    }
    let (c, open) = checkout_access::open_for_order(tx, ctx, token).await?;
    if !open {
        return Ok(None);
    }
    let Some(expected) = linked_account(tx, c.id).await? else {
        return Ok(None);
    };
    checkout::check_review(tx, c.id, input).await?;
    sqlx::query!(
        "DELETE FROM checkout_placements
         WHERE consumed_at IS NULL AND (cart_id = $1 OR expires_at < now())",
        c.id
    )
    .execute(&mut **tx)
    .await?;
    let state = capability::mint();
    sqlx::query!(
        "INSERT INTO checkout_placements
             (state_hash, tenant_id, cart_id, capability_hash, origin, generation,
              expected_customer_id, input, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now() + make_interval(secs => $9))",
        state.hash,
        tx.tenant_id(),
        c.id,
        capability::hash(token),
        p.origin,
        p.generation,
        expected,
        serde_json::to_value(input).map_err(|e| Error::Internal(e.to_string()))?,
        PLACEMENT_TTL_SECS
    )
    .execute(&mut **tx)
    .await?;
    Ok(Some(state.token))
}

#[derive(Debug)]
pub struct Confirmed {
    pub placement: Placement,
    /// The account origin vouched for no account or another one than the expected account.
    pub as_guest: bool,
}

/// A2.4 callback: consumes the placement `state` (unused, unexpired, bound to the presented
/// capability, the exact origin and its current generation) and the account origin's placement
/// `code` issued for it, and places the order with the input the state was created with, all
/// in the caller's transaction. The order belongs to the expected account only when the code
/// vouches for that account; for no account or another one it is a guest order. Anything else
/// is `404` and places nothing: never a guest fallback.
pub async fn confirm_placement(
    tx: &mut TenantTx,
    ctx: &Context,
    settings: &Settings,
    token: &str,
    state: &str,
    code: &str,
    placer: &Placer<'_>,
) -> Result<Confirmed, Error> {
    if !capability::well_formed(state) {
        return Err(Error::NotFound);
    }
    let (c, p) = open_same_origin(tx, ctx, token).await?;
    let origin = ctx.origin.as_ref().ok_or(Error::NotFound)?;
    let evidence = account_codes::consume(tx, Purpose::Placement, state, origin, code)
        .await?
        .ok_or(Error::NotFound)?;
    let row = sqlx::query!(
        r#"UPDATE checkout_placements
           SET consumed_at = now(), as_guest = expected_customer_id IS DISTINCT FROM $6
           WHERE state_hash = $1 AND cart_id = $2 AND capability_hash = $3
             AND origin = $4 AND generation = $5
             AND consumed_at IS NULL AND expires_at > now()
           RETURNING expected_customer_id, input, as_guest AS "as_guest!""#,
        capability::hash(state),
        c.id,
        capability::hash(token),
        p.origin,
        p.generation,
        evidence.customer_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let input: PlaceOrderInput = serde_json::from_value(row.input.clone())
        .map_err(|e| Error::Internal(format!("stored placement input: {e}")))?;
    let key = format!("placement:{}", hex::encode(capability::hash(state)));
    let placement = checkout::place_order(
        tx,
        ctx,
        settings,
        token,
        &key,
        &idempotency::request_hash(row.input.to_string().as_bytes()),
        &input,
        &Placer {
            customer_id: (!row.as_guest).then_some(row.expected_customer_id),
            ip_hash: placer.ip_hash,
        },
    )
    .await?;
    Ok(Confirmed {
        placement,
        as_guest: row.as_guest,
    })
}
