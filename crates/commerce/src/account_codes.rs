use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::capability;
use crate::checkout_access::ShopOrigin;

pub const TTL_SECS: f64 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Identify,
    Placement,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identify => "identify",
            Self::Placement => "placement",
        }
    }
}

/// What a consumed code proves: the account signed in on the account origin, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountEvidence {
    pub customer_id: Option<Uuid>,
}

/// A callback the account origin may send a code to: a same-origin checkout origin of the
/// market, and a well-formed `state`.
#[derive(Debug, Clone)]
pub struct Callback {
    pub purpose: Purpose,
    pub state: String,
    pub origin: ShopOrigin,
    market_id: Uuid,
}

fn state_ok(state: &str) -> bool {
    (16..=128).contains(&state.len())
        && state
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `400 invalid_callback` unless `origin` is exactly one of `market_id`'s checkout origins in
/// same-origin mode, on a verified domain.
pub async fn callback(
    tx: &mut TenantTx,
    market_id: Uuid,
    purpose: Purpose,
    state: &str,
    origin: &str,
) -> Result<Callback, Error> {
    let invalid = || Error::BadRequest {
        code: "invalid_callback",
        detail: "origin must be a same-origin checkout origin of this market".into(),
    };
    if !state_ok(state) {
        return Err(invalid());
    }
    let origin = ShopOrigin::parse(origin).ok_or_else(invalid)?;
    let registered = sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM platform.checkout_origins o
               JOIN platform.domains d USING (hostname, tenant_id, market_id)
               WHERE o.origin = $1 AND o.tenant_id = $2 AND o.market_id = $3
                 AND o.mode = 'same_origin' AND d.verified_at IS NOT NULL
           ) AS "registered!""#,
        origin.as_str(),
        tx.tenant_id(),
        market_id
    )
    .fetch_one(&mut **tx)
    .await?;
    if !registered {
        return Err(invalid());
    }
    Ok(Callback {
        purpose,
        state: state.to_owned(),
        origin,
        market_id,
    })
}

/// Issues a code for `customer` (the account origin's session, if any). An identify code needs
/// a signed-in customer (`401 not_signed_in`); a placement code may carry none.
pub async fn issue(
    tx: &mut TenantTx,
    cb: &Callback,
    customer: Option<Uuid>,
) -> Result<String, Error> {
    if cb.purpose == Purpose::Identify && customer.is_none() {
        return Err(Error::Unauthorized {
            code: "not_signed_in",
        });
    }
    let code = capability::mint();
    sqlx::query!(
        "INSERT INTO account_codes (code_hash, tenant_id, market_id, purpose, state_hash,
                                    callback_origin, customer_id, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, now() + make_interval(secs => $8))",
        code.hash,
        tx.tenant_id(),
        cb.market_id,
        cb.purpose.as_str(),
        capability::hash(&cb.state),
        cb.origin.as_str(),
        customer,
        TTL_SECS
    )
    .execute(&mut **tx)
    .await?;
    Ok(code.token)
}

/// Consumes `code` in the caller's transaction. `None` for a wrong, reused or expired code, or
/// one issued for another purpose, state, origin or tenant.
pub async fn consume(
    tx: &mut TenantTx,
    purpose: Purpose,
    state: &str,
    origin: &ShopOrigin,
    code: &str,
) -> Result<Option<AccountEvidence>, Error> {
    if !capability::well_formed(code) {
        return Ok(None);
    }
    let row = sqlx::query!(
        "UPDATE account_codes SET consumed_at = now()
         WHERE tenant_id = $1 AND code_hash = $2 AND purpose = $3 AND state_hash = $4
           AND callback_origin = $5 AND expires_at > now() AND consumed_at IS NULL
         RETURNING customer_id",
        tx.tenant_id(),
        capability::hash(code),
        purpose.as_str(),
        capability::hash(state),
        origin.as_str()
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| AccountEvidence {
        customer_id: r.customer_id,
    }))
}
