//! Link grants (decision C5): an emailed bearer link opened on a same-origin shop becomes a
//! short-lived browser grant for one resource and action, bound to the tenant, the exact origin
//! and the checkout generation (which changes with the origin's mode). The bearer link itself
//! is never consumed by the exchange. A grant idles out after 30 minutes, never outlives its
//! link, is revoked when the browser opens a newer link and, for single-use actions, when the
//! action completes in the caller's transaction.

use chrono::{DateTime, Duration, Utc};
use platform::Error;
use platform::db::TenantTx;
use uuid::Uuid;

use crate::capability;
use crate::checkout_access::{Policy, enter, require};
use crate::storefront::Context;
use crate::tenancy::CheckoutMode;

pub const IDLE_MINUTES: i64 = 30;
const ANONYMOUS_HOURS: i64 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    OrderRead(Uuid),
    /// The public withdrawal form (decision default 8): one emailed-link request, no reads.
    WithdrawalRequest,
}

impl Scope {
    fn columns(self) -> (&'static str, Option<Uuid>) {
        match self {
            Scope::OrderRead(id) => ("order_read", Some(id)),
            Scope::WithdrawalRequest => ("withdrawal_request", None),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    OrderLink(&'a str),
    WithdrawalForm,
}

#[derive(Debug)]
pub struct Granted {
    pub token: String,
    pub scope: Scope,
}

/// Exchanges `source` for a new grant and revokes the browser's `previous` grant on this origin.
/// `previous` is a revocation hint, never authorization. An unknown or expired link is `404`
/// and revokes nothing; an isolated origin is `409 checkout_mode_changed`.
pub async fn exchange(
    tx: &mut TenantTx,
    ctx: &Context,
    source: Source<'_>,
    previous: Option<&str>,
) -> Result<Granted, Error> {
    let p = enter(tx, ctx).await?;
    require(&p, CheckoutMode::SameOrigin)?;
    let (scope, link_hash, expires_at): (Scope, Option<Vec<u8>>, DateTime<Utc>) = match source {
        Source::OrderLink(token) => {
            if !capability::well_formed(token) {
                return Err(Error::NotFound);
            }
            let hash = capability::hash(token);
            let row = sqlx::query!(
                "SELECT order_id, expires_at FROM order_tokens
                 WHERE token_hash = $1 AND expires_at > now()",
                hash.as_slice()
            )
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(Error::NotFound)?;
            (
                Scope::OrderRead(row.order_id),
                Some(hash.to_vec()),
                row.expires_at,
            )
        }
        Source::WithdrawalForm => (
            Scope::WithdrawalRequest,
            None,
            Utc::now() + Duration::hours(ANONYMOUS_HOURS),
        ),
    };
    if let Some(previous) = previous.filter(|t| capability::well_formed(t)) {
        sqlx::query!(
            "DELETE FROM link_grants WHERE token_hash = $1 AND origin = $2",
            capability::hash(previous),
            p.origin
        )
        .execute(&mut **tx)
        .await?;
    }
    let minted = capability::mint();
    let (kind, order_id) = scope.columns();
    sqlx::query!(
        "INSERT INTO link_grants
             (token_hash, tenant_id, scope, order_id, link_hash, origin, generation, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        minted.hash,
        tx.tenant_id(),
        kind,
        order_id,
        link_hash,
        p.origin,
        p.generation,
        expires_at
    )
    .execute(&mut **tx)
    .await?;
    Ok(Granted {
        token: minted.token,
        scope,
    })
}

/// Checks `grant` for exactly `scope` on this origin and generation, locking it until commit,
/// and refreshes its idle expiry. Anything else is `404` and refreshes nothing.
pub async fn authorize(
    tx: &mut TenantTx,
    ctx: &Context,
    grant: &str,
    scope: Scope,
) -> Result<(), Error> {
    let p = live(tx, ctx, grant).await?;
    let (kind, order_id) = scope.columns();
    sqlx::query_scalar!(
        r#"UPDATE link_grants SET last_activity_at = now()
           WHERE token_hash = $1 AND scope = $2 AND order_id IS NOT DISTINCT FROM $3
             AND origin = $4 AND generation = $5
             AND last_activity_at > now() - make_interval(mins => $6) AND expires_at > now()
           RETURNING 1 AS "one!""#,
        capability::hash(grant),
        kind,
        order_id,
        p.origin,
        p.generation,
        IDLE_MINUTES as i32
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(|_| ())
    .ok_or(Error::NotFound)
}

/// As [`authorize`], but revokes the grant: the single-use action it allows completes in the
/// caller's transaction, so a rollback keeps the grant.
pub async fn complete(
    tx: &mut TenantTx,
    ctx: &Context,
    grant: &str,
    scope: Scope,
) -> Result<(), Error> {
    let p = live(tx, ctx, grant).await?;
    let (kind, order_id) = scope.columns();
    sqlx::query_scalar!(
        r#"DELETE FROM link_grants
           WHERE token_hash = $1 AND scope = $2 AND order_id IS NOT DISTINCT FROM $3
             AND origin = $4 AND generation = $5
             AND last_activity_at > now() - make_interval(mins => $6) AND expires_at > now()
           RETURNING 1 AS "one!""#,
        capability::hash(grant),
        kind,
        order_id,
        p.origin,
        p.generation,
        IDLE_MINUTES as i32
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(|_| ())
    .ok_or(Error::NotFound)
}

async fn live<'a>(tx: &mut TenantTx, ctx: &'a Context, grant: &str) -> Result<Policy<'a>, Error> {
    let p = enter(tx, ctx).await?;
    if p.mode != CheckoutMode::SameOrigin || !capability::well_formed(grant) {
        return Err(Error::NotFound);
    }
    Ok(p)
}
