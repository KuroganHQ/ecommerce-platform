//! Sign-in from a same-origin checkout, checkout half (decision A2 steps 1-3). The shop never
//! holds an account session: identify only links the checkout to the account the account
//! origin vouched for, as a prefill hint for its saved addresses. It never makes the checkout
//! visible to that account and never writes to it.

use platform::Error;
use platform::db::TenantTx;

use crate::account_codes::{self, Purpose};
use crate::capability;
use crate::checkout_access::{bind, open_same_origin};
use crate::customers::{self, Address};
use crate::storefront::Context;

pub const STATE_TTL_SECS: f64 = 300.0;

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
    let customer = sqlx::query_scalar!(
        "SELECT customer_id FROM checkout_identities
         WHERE cart_id = $1 AND consumed_at IS NOT NULL
         ORDER BY consumed_at DESC LIMIT 1",
        c.id
    )
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    match customer {
        Some(id) => customers::addresses(tx, id).await,
        None => Ok(vec![]),
    }
}
