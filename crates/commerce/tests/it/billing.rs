use std::sync::Arc;

use axum::extract::Path;
use chrono::Utc;
use commerce::billing::{self, Billing};
use commerce::payments::stripe::{Processed, Stripe};
use platform::config::{BillingConfig, StripeConfig, StripeMode};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

const SECRET: &str = "whsec_billing_test_0123456789";

/// Stripe's subscription retrieval, answering whatever the test last stored.
async fn fake_stripe(subscription: Arc<RwLock<Value>>) -> String {
    let router = axum::Router::new().route(
        "/v1/subscriptions/{id}",
        axum::routing::get(move |Path(id): Path<String>| {
            let sub = subscription.clone();
            async move {
                let mut body = sub.read().await.clone();
                body["id"] = json!(id);
                axum::Json(body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    url
}

fn stripe(api: &str, webhook_secret: &str) -> Stripe {
    Stripe::new(
        &StripeConfig {
            mode: StripeMode::Simulator,
            api_url: api.parse().unwrap(),
            secret_key: "sk_test_x".into(),
            publishable_key: None,
            webhook_secret: webhook_secret.into(),
        },
        reqwest::Client::new(),
    )
}

struct Fixture {
    db: PgPool,
    owner: PgPool,
    billing: Billing,
    signer: Stripe,
    subscription: Arc<RwLock<Value>>,
    tenant: Uuid,
}

async fn fixture(owner: &PgPool) -> Fixture {
    let db = testkit::runtime_pool(owner, 4).await;
    let subscription = Arc::new(RwLock::new(Value::Null));
    let api = fake_stripe(subscription.clone()).await;
    let billing = Billing::new(
        Some(&stripe(&api, "whsec_connect_0123456789")),
        &BillingConfig {
            plans: vec![("pro".into(), "price_pro".into())],
            webhook_secret: Some(SECRET.into()),
        },
    )
    .unwrap();
    let (tenant, _) = testkit::tenant(&db, "billed").await;
    sqlx::query(
        "INSERT INTO platform.subscriptions (tenant_id, stripe_customer_id) VALUES ($1, 'cus_ours')",
    )
    .bind(tenant)
    .execute(&db)
    .await
    .unwrap();
    Fixture {
        db,
        owner: owner.clone(),
        billing,
        signer: stripe(&api, SECRET),
        subscription,
        tenant,
    }
}

impl Fixture {
    async fn set_subscription(&self, status: &str, customer: &str) {
        *self.subscription.write().await = json!({
            "object": "subscription",
            "customer": customer,
            "status": status,
            "cancel_at_period_end": false,
            "items": { "data": [{ "price": { "id": "price_pro" }, "current_period_end": 1_900_000_000 }] },
        });
    }

    /// Delivers an event through the webhook path and processes the stored row.
    async fn deliver(&self, event_id: &str, kind: &str, object: Value) -> Processed {
        let raw = json!({ "id": event_id, "type": kind, "livemode": false, "data": { "object": object } })
            .to_string();
        let sig = self.signer.sign(raw.as_bytes(), Utc::now()).unwrap();
        assert!(
            billing::receive(&self.db, &self.billing, &sig, raw.as_bytes())
                .await
                .unwrap()
        );
        let id: Uuid =
            sqlx::query_scalar("SELECT id FROM platform.provider_events WHERE event_id = $1")
                .bind(event_id)
                .fetch_one(&self.db)
                .await
                .unwrap();
        let job: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM queue.jobs WHERE kind = $1 AND payload->>'id' = $2",
        )
        .bind(billing::EVENT_JOB)
        .bind(id.to_string())
        .fetch_one(&self.owner)
        .await
        .unwrap();
        assert_eq!(job, 1, "the billing job is enqueued");
        billing::process_event(&self.db, &self.billing, id)
            .await
            .unwrap()
    }

    async fn plan(&self) -> (String, Option<String>) {
        sqlx::query_as(
            "SELECT t.plan, s.status FROM platform.tenants t
             JOIN platform.subscriptions s ON s.tenant_id = t.id WHERE t.id = $1",
        )
        .bind(self.tenant)
        .fetch_one(&self.db)
        .await
        .unwrap()
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn subscription_events_set_the_plan_from_stripe(db: PgPool) {
    let f = fixture(&db).await;
    f.set_subscription("active", "cus_ours").await;
    let outcome = f
        .deliver(
            "evt_1",
            "checkout.session.completed",
            json!({ "object": "checkout.session", "customer": "cus_ours", "subscription": "sub_1" }),
        )
        .await;
    assert!(matches!(outcome, Processed::Applied(_)), "{outcome:?}");
    assert_eq!(f.plan().await, ("pro".into(), Some("active".into())));

    // The event says "updated", but Stripe now says canceled: Stripe wins.
    f.set_subscription("canceled", "cus_ours").await;
    f.deliver(
        "evt_2",
        "customer.subscription.deleted",
        json!({ "object": "subscription", "id": "sub_1", "customer": "cus_ours", "status": "active" }),
    )
    .await;
    assert_eq!(f.plan().await, ("standard".into(), Some("canceled".into())));

    // Processing a stored event again changes nothing.
    let id: Uuid =
        sqlx::query_scalar("SELECT id FROM platform.provider_events WHERE event_id = 'evt_1'")
            .fetch_one(&f.db)
            .await
            .unwrap();
    billing::process_event(&f.db, &f.billing, id).await.unwrap();
    assert_eq!(f.plan().await, ("standard".into(), Some("canceled".into())));
}

#[sqlx::test(migrations = "../../migrations")]
async fn events_of_unknown_customers_are_ignored(db: PgPool) {
    let f = fixture(&db).await;
    f.set_subscription("active", "cus_theirs").await;
    let outcome = f
        .deliver(
            "evt_x",
            "customer.subscription.updated",
            json!({ "id": "sub_x", "customer": "cus_theirs", "metadata": { "tenant_id": f.tenant } }),
        )
        .await;
    assert_eq!(outcome, Processed::Ignored("unknown customer".into()));
    assert_eq!(f.plan().await.0, "standard");

    // Our customer in the event, someone else's on the subscription Stripe returns.
    let outcome = f
        .deliver(
            "evt_y",
            "customer.subscription.updated",
            json!({ "id": "sub_y", "customer": "cus_ours" }),
        )
        .await;
    assert_eq!(outcome, Processed::Rejected("customer mismatch".into()));
    assert_eq!(f.plan().await.0, "standard");
}

#[sqlx::test(migrations = "../../migrations")]
async fn billing_events_need_the_billing_secret(db: PgPool) {
    let f = fixture(&db).await;
    let raw = br#"{"id":"evt_1","type":"customer.subscription.updated","livemode":false}"#;
    let connect_signed = stripe("http://127.0.0.1:1", "whsec_connect_0123456789")
        .sign(raw, Utc::now())
        .unwrap();
    let err = billing::receive(&f.db, &f.billing, &connect_signed, raw)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_signature");
}
