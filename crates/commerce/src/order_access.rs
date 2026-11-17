//! Who may read or pay an order on a same-origin shop (decision C4/C5): the checkout capability
//! the order was placed with, in the origin's current checkout generation, may pay; a link grant
//! for the order may only read. Paying as the order's customer is SHOP-95's account origin, so
//! the shop origin accepts no session here.

use platform::Error;
use platform::db::TenantTx;
use uuid::Uuid;

use crate::checkout_access::{converted, enter};
use crate::link_access::{self, Scope};
use crate::orders;
use crate::storefront::Context;
use crate::tenancy::CheckoutMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Pay,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Credentials<'a> {
    pub checkout: Option<&'a str>,
    pub grant: Option<&'a str>,
}

/// What `credentials` allow on `order`, `404` when nothing. Same-origin mode only; each check
/// runs in the caller's transaction and locks what it read until commit.
pub async fn authorize(
    tx: &mut TenantTx,
    ctx: &Context,
    order: Uuid,
    credentials: Credentials<'_>,
) -> Result<Access, Error> {
    if let Some(checkout) = credentials.checkout
        && pays(tx, ctx, order, checkout).await?
    {
        return Ok(Access::Pay);
    }
    let grant = credentials.grant.ok_or(Error::NotFound)?;
    link_access::authorize(tx, ctx, grant, Scope::OrderRead(order)).await?;
    Ok(Access::Read)
}

/// Whether `checkout` may pay `order`. An isolated shop keeps its rule: the checkout capability
/// the order was placed with, or the session of the order's customer.
pub async fn may_pay(
    tx: &mut TenantTx,
    ctx: &Context,
    order: Uuid,
    checkout: Option<&str>,
    customer: Option<Uuid>,
) -> Result<bool, Error> {
    if same_origin(tx, ctx).await? {
        return match checkout {
            Some(checkout) => pays(tx, ctx, order, checkout).await,
            None => Ok(false),
        };
    }
    orders::may_pay(tx, order, checkout, customer).await
}

/// The order page a payment returns to: the clean `/o/<id>` on a same-origin shop, else the
/// email link.
pub async fn page(
    tx: &mut TenantTx,
    ctx: &Context,
    order: Uuid,
    token: &str,
) -> Result<String, Error> {
    Ok(if same_origin(tx, ctx).await? {
        format!("/o/{order}")
    } else {
        format!("/o/{token}")
    })
}

async fn same_origin(tx: &mut TenantTx, ctx: &Context) -> Result<bool, Error> {
    Ok(ctx.origin.is_some() && enter(tx, ctx).await?.mode == CheckoutMode::SameOrigin)
}

async fn pays(
    tx: &mut TenantTx,
    ctx: &Context,
    order: Uuid,
    checkout: &str,
) -> Result<bool, Error> {
    let cart = match converted(tx, ctx, checkout).await {
        Ok(cart) => cart,
        Err(Error::NotFound) => return Ok(false),
        Err(e) => return Err(e),
    };
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM orders WHERE id = $1 AND cart_id = $2) AS "placed!""#,
        order,
        cart.id
    )
    .fetch_one(&mut **tx)
    .await?)
}
