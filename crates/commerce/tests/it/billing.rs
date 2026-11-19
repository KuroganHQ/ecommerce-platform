use std::sync::Arc;

use axum::extract::Path;
use axum::routing::{get, post};
use chrono::Utc;
use commerce::billing::{self, Billing};
use commerce::payments::stripe::{Processed, Stripe};
use platform::config::{BillingConfig, StripeConfig, StripeMode};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::{Mutex, RwLock, oneshot};
use uuid::Uuid;

const SECRET: &str = "whsec_billing_test_0123456789";

#[derive(Default)]
struct FakeStripe {
    subscription: RwLock<Value>,
    sessions: RwLock<Vec<&'static str>>,
    held_read: Mutex<Option<HeldRead>>,
}

struct HeldRead {
    snapshot: oneshot::Sender<Value>,
    release: oneshot::Receiver<()>,
}

fn session_index(id: &str) -> usize {
    id.trim_start_matches("cs_").parse().unwrap()
}

async fn fake_stripe(fake: Arc<FakeStripe>) -> String {
    let (f1, f2, f3, f4) = (fake.clone(), fake.clone(), fake.clone(), fake);
    let router = axum::Router::new()
        .route(
            "/v1/subscriptions/{id}",
            get(move |Path(id): Path<String>| {
                let fake = f1.clone();
                async move {
                    let mut body = fake.subscription.read().await.clone();
                    body["id"] = json!(id);
                    let held = fake.held_read.lock().await.take();
                    if let Some(held) = held {
                        held.snapshot.send(body.clone()).unwrap();
                        held.release.await.unwrap();
                    }
                    axum::Json(body)
                }
            }),
        )
        .route(
            "/v1/checkout/sessions",
            post(move || {
                let fake = f2.clone();
                async move {
                    let mut sessions = fake.sessions.write().await;
                    sessions.push("open");
                    let id = format!("cs_{}", sessions.len() - 1);
                    axum::Json(
                        json!({ "id": id, "url": format!("https://checkout.stripe.test/{id}") }),
                    )
                }
            }),
        )
        .route(
            "/v1/checkout/sessions/{id}",
            get(move |Path(id): Path<String>| {
                let fake = f3.clone();
                async move {
                    let index = session_index(&id);
                    let status = fake.sessions.read().await[index];
                    let subscription = (status == "complete").then(|| format!("sub_{}", index + 1));
                    axum::Json(json!({ "id": id, "status": status, "subscription": subscription }))
                }
            }),
        )
        .route(
            "/v1/checkout/sessions/{id}/expire",
            post(move |Path(id): Path<String>| {
                let fake = f4.clone();
                async move {
                    let mut sessions = fake.sessions.write().await;
                    let status = &mut sessions[session_index(&id)];
                    assert_eq!(*status, "open", "only an open session can be expired");
                    *status = "expired";
                    axum::Json(json!({ "id": id, "status": "expired" }))
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
    stripe: Arc<FakeStripe>,
    tenant: Uuid,
}

async fn fixture(owner: &PgPool) -> Fixture {
    let db = testkit::runtime_pool(owner, 4).await;
    let fake = Arc::new(FakeStripe::default());
    let api = fake_stripe(fake.clone()).await;
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
        stripe: fake,
        tenant,
    }
}

impl Fixture {
    async fn set_subscription(&self, status: &str, customer: &str) {
        *self.stripe.subscription.write().await = json!({
            "object": "subscription",
            "customer": customer,
            "status": status,
            "cancel_at_period_end": false,
            "items": { "data": [{ "price": { "id": "price_pro" }, "current_period_end": 1_900_000_000 }] },
        });
    }

    async fn deliver(&self, event_id: &str, kind: &str, object: Value) -> Processed {
        let id = self.receive_unprocessed(event_id, kind, object).await;
        billing::process_event(&self.db, &self.billing, id)
            .await
            .unwrap()
    }

    async fn receive_unprocessed(&self, event_id: &str, kind: &str, object: Value) -> Uuid {
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
        id
    }

    async fn checkout(&self) -> Result<String, platform::Error> {
        billing::checkout(
            &self.db,
            Some(&self.billing),
            self.tenant,
            "owner@example.test",
            "pro",
            "https://admin.example.test",
        )
        .await
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

    f.set_subscription("canceled", "cus_ours").await;
    f.deliver(
        "evt_2",
        "customer.subscription.deleted",
        json!({ "object": "subscription", "id": "sub_1", "customer": "cus_ours", "status": "active" }),
    )
    .await;
    assert_eq!(f.plan().await, ("standard".into(), Some("canceled".into())));

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

#[sqlx::test(migrations = "../../migrations")]
async fn overlapping_events_keep_the_newest_stripe_state(db: PgPool) {
    let f = Arc::new(fixture(&db).await);
    let object = json!({ "id": "sub_1", "customer": "cus_ours" });
    let older = f
        .receive_unprocessed("evt_older", "customer.subscription.updated", object.clone())
        .await;
    let newer = f
        .receive_unprocessed("evt_newer", "customer.subscription.updated", object)
        .await;

    f.set_subscription("active", "cus_ours").await;
    let (snapshot_tx, snapshot) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    *f.stripe.held_read.lock().await = Some(HeldRead {
        snapshot: snapshot_tx,
        release: release_rx,
    });
    let first = {
        let f = f.clone();
        tokio::spawn(async move { billing::process_event(&f.db, &f.billing, older).await })
    };
    assert_eq!(
        snapshot.await.unwrap()["status"],
        "active",
        "the older job read Stripe"
    );

    f.set_subscription("canceled", "cus_ours").await;
    let mut second = {
        let f = f.clone();
        tokio::spawn(async move { billing::process_event(&f.db, &f.billing, newer).await })
    };
    let early = tokio::time::timeout(std::time::Duration::from_millis(500), &mut second).await;
    release.send(()).unwrap();
    first.await.unwrap().unwrap();
    match early {
        Ok(done) => done.unwrap().unwrap(),
        Err(_) => second.await.unwrap().unwrap(),
    };

    assert_eq!(f.plan().await, ("standard".into(), Some("canceled".into())));
}

#[sqlx::test(migrations = "../../migrations")]
async fn choosing_a_plan_again_expires_the_open_checkout(db: PgPool) {
    let f = fixture(&db).await;
    let first = f.checkout().await.unwrap();
    let second = f.checkout().await.unwrap();
    assert_eq!(first, "https://checkout.stripe.test/cs_0");
    assert_eq!(second, "https://checkout.stripe.test/cs_1");
    assert_eq!(
        *f.stripe.sessions.read().await,
        ["expired", "open"],
        "only the newest Checkout can still be paid"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_paid_checkout_refuses_another_before_its_webhook(db: PgPool) {
    let f = fixture(&db).await;
    f.checkout().await.unwrap();
    f.stripe.sessions.write().await[0] = "complete";
    f.set_subscription("active", "cus_ours").await;
    let err = f.checkout().await.unwrap_err();
    assert_eq!(err.code(), "subscription_exists");
    assert_eq!(
        *f.stripe.sessions.read().await,
        ["complete"],
        "no second Checkout"
    );

    f.set_subscription("canceled", "cus_ours").await;
    f.deliver(
        "evt_ended",
        "customer.subscription.deleted",
        json!({ "id": "sub_1", "customer": "cus_ours" }),
    )
    .await;
    assert!(f.checkout().await.is_ok());
    assert_eq!(*f.stripe.sessions.read().await, ["complete", "open"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_late_event_of_an_ended_subscription_keeps_the_store_billable(db: PgPool) {
    let f = fixture(&db).await;
    f.set_subscription("canceled", "cus_ours").await;
    for (session, subscription) in [(0, "sub_1"), (1, "sub_2")] {
        f.checkout().await.unwrap();
        f.stripe.sessions.write().await[session] = "complete";
        f.deliver(
            &format!("evt_{subscription}_ended"),
            "customer.subscription.deleted",
            json!({ "id": subscription, "customer": "cus_ours" }),
        )
        .await;
    }
    f.deliver(
        "evt_sub_1_late",
        "customer.subscription.updated",
        json!({ "id": "sub_1", "customer": "cus_ours" }),
    )
    .await;
    assert!(
        f.checkout().await.is_ok(),
        "both subscriptions ended, so the store can subscribe again"
    );
}
