//! Store subscriptions (SHOP-45): Stripe Checkout and the customer portal on the platform's own
//! Stripe account, and the billing webhook that keeps `tenants.plan` in sync. Stripe is the
//! source of truth: every event re-reads the subscription, and the tenant comes from our
//! `stripe_customer_id` mapping, never from event metadata.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::config::BillingConfig;
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::payments::stripe::{self, Processed, Stripe};

pub const EVENT_JOB: &str = "billing.stripe_event";
pub const DEFAULT_PLAN: &str = "standard";

const PAID: &[&str] = &["trialing", "active", "past_due"];
const OPEN: &[&str] = &["trialing", "active", "past_due", "unpaid", "paused"];

pub fn plan_for<'a>(status: &str, subscribed: Option<&'a str>) -> &'a str {
    match subscribed {
        Some(plan) if PAID.contains(&status) => plan,
        _ => DEFAULT_PLAN,
    }
}

#[derive(Clone)]
pub struct Billing {
    stripe: Stripe,
    plans: Vec<(String, String)>,
}

impl Billing {
    pub fn new(stripe: Option<&Stripe>, cfg: &BillingConfig) -> Option<Self> {
        let secret = cfg.webhook_secret.as_deref()?;
        if cfg.plans.is_empty() {
            return None;
        }
        Some(Self {
            stripe: stripe?.with_webhook_secret(secret),
            plans: cfg.plans.clone(),
        })
    }

    fn price(&self, plan: &str) -> Option<&str> {
        self.plans
            .iter()
            .find(|(p, _)| p == plan)
            .map(|(_, price)| price.as_str())
    }

    fn plan_of_price(&self, price: &str) -> Option<&str> {
        self.plans
            .iter()
            .find(|(_, p)| p == price)
            .map(|(plan, _)| plan.as_str())
    }
}

fn not_configured() -> Error {
    Error::NotConfigured {
        code: "billing_not_configured",
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BillingOverview {
    /// `false`: billing is not set up (`BILLING_PLANS`, Stripe); checkout and portal answer 503.
    pub configured: bool,
    /// The store's current plan (`standard` without a paid subscription).
    pub plan: String,
    /// Stripe's subscription status (`active`, `past_due`, `canceled`, ...); `null` before the
    /// first subscription.
    pub status: Option<String>,
    pub current_period_end: Option<DateTime<Utc>>,
    pub cancel_at_period_end: bool,
    /// The subscription still exists: plan changes and cancellation go through the portal, and
    /// checkout answers `409 subscription_exists`.
    pub subscription_open: bool,
    /// Plans the store can choose.
    pub plans: Vec<String>,
}

pub async fn overview(
    db: &PgPool,
    billing: Option<&Billing>,
    tenant_id: Uuid,
) -> Result<BillingOverview, Error> {
    let row = sqlx::query!(
        r#"SELECT t.plan, s.status AS "status?", s.current_period_end AS "current_period_end?",
                  s.cancel_at_period_end AS "cancel_at_period_end?"
           FROM platform.tenants t LEFT JOIN platform.subscriptions s ON s.tenant_id = t.id
           WHERE t.id = $1"#,
        tenant_id
    )
    .fetch_optional(db)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(BillingOverview {
        configured: billing.is_some(),
        plan: row.plan,
        subscription_open: row.status.as_deref().is_some_and(|s| OPEN.contains(&s)),
        status: row.status,
        current_period_end: row.current_period_end,
        cancel_at_period_end: row.cancel_at_period_end.unwrap_or(false),
        plans: billing
            .map(|b| b.plans.iter().map(|(p, _)| p.clone()).collect())
            .unwrap_or_default(),
    })
}

pub async fn checkout(
    db: &PgPool,
    billing: Option<&Billing>,
    tenant_id: Uuid,
    owner_email: &str,
    plan: &str,
    admin_origin: &str,
) -> Result<String, Error> {
    let billing = billing.ok_or_else(not_configured)?;
    let price = billing.price(plan).ok_or_else(|| Error::Validation {
        code: "unknown_plan",
        detail: format!("no plan {plan:?}"),
    })?;
    let customer = customer(db, billing, tenant_id, owner_email).await?;
    let form = [
        ("mode", "subscription"),
        ("customer", customer.as_str()),
        ("line_items[0][price]", price),
        ("line_items[0][quantity]", "1"),
        ("client_reference_id", &tenant_id.to_string()),
        (
            "success_url",
            &format!("{admin_origin}/billing?checkout=done"),
        ),
        ("cancel_url", &format!("{admin_origin}/billing")),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()));
    let mut tx = db.begin().await?;
    let row = sqlx::query!(
        "SELECT status, stripe_subscription_id, checkout_session_id
         FROM platform.subscriptions WHERE tenant_id = $1 FOR UPDATE",
        tenant_id
    )
    .fetch_one(&mut *tx)
    .await?;
    if row.status.as_deref().is_some_and(|s| OPEN.contains(&s)) {
        return Err(subscription_exists());
    }
    if let Some(previous) = &row.checkout_session_id {
        let path = format!("/v1/checkout/sessions/{previous}");
        let session = billing
            .stripe
            .call(reqwest::Method::GET, &path, None, None, &[])
            .await?;
        match session.get("status").and_then(Value::as_str) {
            Some("open") => {
                billing
                    .stripe
                    .call(
                        reqwest::Method::POST,
                        &format!("{path}/expire"),
                        None,
                        None,
                        &[],
                    )
                    .await?;
            }
            Some("complete") => {
                let paid = session.get("subscription").and_then(Value::as_str);
                if let Some(paid) =
                    paid.filter(|p| Some(*p) != row.stripe_subscription_id.as_deref())
                {
                    let sub = billing
                        .stripe
                        .call(
                            reqwest::Method::GET,
                            &format!("/v1/subscriptions/{paid}"),
                            None,
                            None,
                            &[],
                        )
                        .await?;
                    if sub
                        .get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|s| OPEN.contains(&s))
                    {
                        return Err(subscription_exists());
                    }
                }
            }
            _ => {}
        }
    }
    let session = billing
        .stripe
        .call(
            reqwest::Method::POST,
            "/v1/checkout/sessions",
            None,
            None,
            &form,
        )
        .await?;
    let session_id = Stripe::str_field(&session, "id")?;
    let url = Stripe::str_field(&session, "url")?;
    sqlx::query!(
        "UPDATE platform.subscriptions SET checkout_session_id = $2 WHERE tenant_id = $1",
        tenant_id,
        session_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(url)
}

fn subscription_exists() -> Error {
    Error::Conflict {
        code: "subscription_exists",
        detail: "the store already has a subscription; manage it in the billing portal".into(),
    }
}

pub async fn portal(
    db: &PgPool,
    billing: Option<&Billing>,
    tenant_id: Uuid,
    admin_origin: &str,
) -> Result<String, Error> {
    let billing = billing.ok_or_else(not_configured)?;
    let customer = sqlx::query_scalar!(
        "SELECT stripe_customer_id FROM platform.subscriptions WHERE tenant_id = $1",
        tenant_id
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| Error::Conflict {
        code: "no_billing_account",
        detail: "choose a plan first".into(),
    })?;
    let form = [
        ("customer", customer),
        ("return_url", format!("{admin_origin}/billing")),
    ]
    .map(|(k, v)| (k.to_owned(), v));
    let session = billing
        .stripe
        .call(
            reqwest::Method::POST,
            "/v1/billing_portal/sessions",
            None,
            None,
            &form,
        )
        .await?;
    Stripe::str_field(&session, "url")
}

async fn customer(
    db: &PgPool,
    billing: &Billing,
    tenant_id: Uuid,
    email: &str,
) -> Result<String, Error> {
    let existing = sqlx::query_scalar!(
        "SELECT stripe_customer_id FROM platform.subscriptions WHERE tenant_id = $1",
        tenant_id
    )
    .fetch_optional(db)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let name = sqlx::query_scalar!("SELECT name FROM platform.tenants WHERE id = $1", tenant_id)
        .fetch_one(db)
        .await?;
    let form = [
        ("email", email.to_owned()),
        ("name", name),
        ("metadata[tenant_id]", tenant_id.to_string()),
    ]
    .map(|(k, v)| (k.to_owned(), v));
    let created = billing
        .stripe
        .call(
            reqwest::Method::POST,
            "/v1/customers",
            None,
            Some(&format!("billing-customer-{tenant_id}")),
            &form,
        )
        .await?;
    let id = Stripe::str_field(&created, "id")?;
    Ok(sqlx::query_scalar!(
        "INSERT INTO platform.subscriptions (tenant_id, stripe_customer_id) VALUES ($1, $2)
         ON CONFLICT (tenant_id) DO UPDATE SET tenant_id = EXCLUDED.tenant_id
         RETURNING stripe_customer_id",
        tenant_id,
        id
    )
    .fetch_one(db)
    .await?)
}

pub async fn receive(
    db: &PgPool,
    billing: &Billing,
    signature: &str,
    raw: &[u8],
) -> Result<bool, Error> {
    stripe::receive_for(db, &billing.stripe, signature, raw, EVENT_JOB).await
}

fn stripe_id(v: &Value) -> Option<&str> {
    v.as_str()
        .filter(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'))
}

fn subscription_ref<'a>(kind: &str, object: &'a Value) -> Option<(&'a str, &'a str)> {
    let field = |name| object.get(name).and_then(stripe_id);
    match kind {
        "checkout.session.completed" => Some((field("subscription")?, field("customer")?)),
        "customer.subscription.created"
        | "customer.subscription.updated"
        | "customer.subscription.deleted" => Some((field("id")?, field("customer")?)),
        _ => None,
    }
}

pub async fn process_event(db: &PgPool, billing: &Billing, id: Uuid) -> Result<Processed, Error> {
    let mut tx = db.begin().await?;
    let ev = sqlx::query!(
        "SELECT type, payload, processed_at, outcome, detail
         FROM platform.provider_events WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    if ev.processed_at.is_some() {
        let d = ev.detail.unwrap_or_default();
        return Ok(match ev.outcome.as_deref() {
            Some("applied") => Processed::Applied(d),
            Some("rejected") => Processed::Rejected(d),
            _ => Processed::Ignored(d),
        });
    }
    let object = ev.payload.pointer("/data/object").unwrap_or(&Value::Null);
    let (tenant_id, outcome) = match subscription_ref(&ev.r#type, object) {
        None => (
            None,
            Processed::Ignored(format!("unhandled type {}", ev.r#type)),
        ),
        Some((subscription_id, customer)) => {
            let current = sqlx::query!(
                "SELECT tenant_id, stripe_subscription_id, status FROM platform.subscriptions
                 WHERE stripe_customer_id = $1 FOR UPDATE",
                customer
            )
            .fetch_optional(&mut *tx)
            .await?;
            match current {
                None => (None, Processed::Ignored("unknown customer".into())),
                Some(current) => {
                    let outcome = apply_subscription(
                        &mut tx,
                        billing,
                        current.tenant_id,
                        subscription_id,
                        customer,
                        current.stripe_subscription_id.as_deref(),
                        current.status.as_deref(),
                    )
                    .await?;
                    (Some(current.tenant_id), outcome)
                }
            }
        }
    };
    stripe::finish(&mut tx, id, tenant_id, &outcome).await?;
    tx.commit().await?;
    Ok(outcome)
}

async fn apply_subscription(
    tx: &mut sqlx::PgConnection,
    billing: &Billing,
    tenant_id: Uuid,
    subscription_id: &str,
    customer: &str,
    stored_subscription: Option<&str>,
    stored_status: Option<&str>,
) -> Result<Processed, Error> {
    let sub = billing
        .stripe
        .call(
            reqwest::Method::GET,
            &format!("/v1/subscriptions/{subscription_id}"),
            None,
            None,
            &[],
        )
        .await?;
    if sub.get("customer").and_then(Value::as_str) != Some(customer) {
        return Ok(Processed::Rejected("customer mismatch".into()));
    }
    let status = Stripe::str_field(&sub, "status")?;
    let replaced = stored_subscription.is_some_and(|s| s != subscription_id);
    let stored_open = stored_status.is_some_and(|s| OPEN.contains(&s));
    if replaced && stored_open && !OPEN.contains(&status.as_str()) {
        return Ok(Processed::Ignored("an older subscription".into()));
    }
    let price = sub
        .pointer("/items/data/0/price/id")
        .and_then(Value::as_str);
    let plan = price.and_then(|p| billing.plan_of_price(p));
    // Newer API versions moved the period end from the subscription to its items.
    let period_end = sub
        .get("current_period_end")
        .and_then(Value::as_i64)
        .or_else(|| {
            sub.pointer("/items/data/0/current_period_end")
                .and_then(Value::as_i64)
        })
        .and_then(|t| DateTime::from_timestamp(t, 0));
    let cancel_at_period_end = sub
        .get("cancel_at_period_end")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    sqlx::query!(
        "UPDATE platform.subscriptions SET stripe_subscription_id = $2, price_id = $3,
             plan = $4, status = $5, current_period_end = $6, cancel_at_period_end = $7,
             updated_at = now()
         WHERE tenant_id = $1",
        tenant_id,
        subscription_id,
        price,
        plan,
        status,
        period_end,
        cancel_at_period_end
    )
    .execute(&mut *tx)
    .await?;
    let effective = plan_for(&status, plan);
    sqlx::query!(
        "UPDATE platform.tenants SET plan = $2 WHERE id = $1",
        tenant_id,
        effective
    )
    .execute(&mut *tx)
    .await?;
    Ok(Processed::Applied(format!("{status}: plan {effective}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_follows_the_subscription_status() {
        for status in ["trialing", "active", "past_due"] {
            assert_eq!(plan_for(status, Some("pro")), "pro", "{status}");
        }
        for status in [
            "canceled",
            "unpaid",
            "incomplete",
            "incomplete_expired",
            "paused",
        ] {
            assert_eq!(plan_for(status, Some("pro")), "standard", "{status}");
        }
        assert_eq!(plan_for("active", None), "standard", "a price no plan maps");
    }
}
