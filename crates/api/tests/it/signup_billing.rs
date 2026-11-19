use std::sync::Arc;
use std::time::Duration;

use crate::common::*;
use axum::http::StatusCode;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn setup(db: &PgPool) -> (api::AppState, Jwks) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let s = state(
        testkit::runtime_pool(db, 4).await,
        &jwks,
        Duration::from_secs(30),
    );
    (s, jwks)
}

fn off(s: &api::AppState) -> api::AppState {
    api::AppState {
        self_signup: None,
        billing: None,
        ..s.clone()
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn signup_routes_are_404_unless_enabled(db: PgPool) {
    let (s, _jwks) = setup(&db).await;
    let (status, body, _) = Call::get("/signup").send(&s).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "enabled": true })));

    let off = off(&s);
    let token = sign(&claims("someone"));
    for call in [
        Call::get("/signup"),
        Call::post("/signup", json!({ "email": "a@example.test" })),
        Call::post("/admin/v1/stores", json!({ "name": "A", "slug": "a" })).token(&token),
    ] {
        let (status, _, _) = call.send(&off).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn signup_answers_alike_for_new_and_known_emails(db: PgPool) {
    let (s, jwks) = setup(&db).await;
    let mut bodies = Vec::new();
    for email in [
        "  New@Example.test ",
        "new@example.test",
        "other@example.test",
    ] {
        let (status, body, _) = Call::post("/signup", json!({ "email": email }))
            .send(&s)
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        bodies.push(body);
    }
    assert!(bodies.windows(2).all(|w| w[0] == w[1]), "{bodies:?}");

    let calls = jwks.auth_requests.read().await;
    let paths: Vec<&str> = calls.iter().map(|(p, _, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/internal/users",
            "/internal/users/invite",
            "/internal/users",
            "/internal/users/invite",
            "/internal/users",
            "/internal/users/invite",
        ]
    );
    assert_eq!(calls[0].2["email"], "new@example.test");
    assert_eq!(calls[1].2["callback_url"], ADMIN_ORIGIN);

    let (status, body, _) = Call::post("/signup", json!({ "email": "not-an-email" }))
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_email");
    let long = format!("{}@example.test", "a".repeat(250));
    let (status, _, _) = Call::post("/signup", json!({ "email": long }))
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let no_auth = api::AppState {
        auth_service: None,
        ..s.clone()
    };
    let (status, _, _) = Call::post("/signup", json!({ "email": "a@example.test" }))
        .send(&no_auth)
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[sqlx::test(migrations = "../../migrations")]
async fn signup_is_rate_limited_per_client_ip(db: PgPool) {
    let (s, jwks) = setup(&db).await;
    let s = api::AppState {
        self_signup: Some(Arc::new(api::rate_limit::StorefrontLimiter::per_hour(2))),
        ..s
    };
    let from = |ip: &'static str| {
        Call::post("/signup", json!({ "email": "a@example.test" })).header("x-client-ip", ip)
    };
    for _ in 0..2 {
        assert_eq!(from("203.0.113.1").send(&s).await.0, StatusCode::ACCEPTED);
    }
    let (status, body, res) = from("203.0.113.1").send(&s).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(res.headers().contains_key("retry-after"));
    assert_eq!(from("203.0.113.2").send(&s).await.0, StatusCode::ACCEPTED);
    assert_eq!(
        jwks.auth_requests.read().await.len(),
        6,
        "a limited request sends nothing"
    );
}

async fn create_store(s: &api::AppState, token: &str, slug: &str) -> (StatusCode, Value) {
    let (status, body, _) = Call::post(
        "/admin/v1/stores",
        json!({ "name": format!("Shop {slug}"), "slug": slug }),
    )
    .token(token)
    .send(s)
    .await;
    (status, body)
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_user_creates_one_store_and_owns_it(db: PgPool) {
    let (s, _jwks) = setup(&db).await;
    let token = sign(&claims("merchant"));
    let (status, body) = create_store(&s, &token, "first-shop").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let tenant: Uuid = body["tenant_id"].as_str().unwrap().parse().unwrap();

    let (_, me, _) = Call::get("/admin/v1/me").token(&token).send(&s).await;
    assert_eq!(me["memberships"][0]["tenant_id"], tenant.to_string());
    assert_eq!(me["memberships"][0]["role"], "owner");
    let (host, created): (String, i64) = sqlx::query_as(
        "SELECT (SELECT hostname FROM platform.domains WHERE tenant_id = $1),
                (SELECT count(*) FROM platform.storefront_tokens WHERE tenant_id = $1)",
    )
    .bind(tenant)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!((host.as_str(), created), ("first-shop.localhost", 1));

    let (status, body) = create_store(&s, &token, "second-shop").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "store_limit_reached");

    let (status, body) = create_store(&s, &sign(&claims("other")), "first-shop").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "already_exists");
    let (status, body) = create_store(&s, &sign(&claims("other")), "admin").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "reserved_slug");
}

#[sqlx::test(migrations = "../../migrations")]
async fn superadmins_create_any_number_of_stores(db: PgPool) {
    let (s, _jwks) = setup(&db).await;
    sqlx::query("INSERT INTO platform.platform_admins (user_id) VALUES ('root')")
        .execute(&db)
        .await
        .unwrap();
    let token = sign(&claims("root"));
    for slug in ["root-one", "root-two"] {
        assert_eq!(create_store(&s, &token, slug).await.0, StatusCode::CREATED);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn creating_a_store_needs_a_verified_staff_token(db: PgPool) {
    let (s, _jwks) = setup(&db).await;
    let (status, _, _) = Call::post("/admin/v1/stores", json!({ "name": "A", "slug": "a" }))
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let mut unverified = claims("merchant");
    unverified["email_verified"] = json!(false);
    let (status, body) = create_store(&s, &sign(&unverified), "unverified").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "email_not_verified");
}

async fn store_with_staff(s: &api::AppState) -> Uuid {
    let (tenant, _) = testkit::tenant(&s.db, "billed").await;
    testkit::staff(&s.db, tenant, "owner", "owner").await;
    testkit::staff(&s.db, tenant, "admin", "admin").await;
    tenant
}

#[sqlx::test(migrations = "../../migrations")]
async fn billing_is_for_owners(db: PgPool) {
    let (s, _jwks) = setup(&db).await;
    let tenant = store_with_staff(&s).await;
    let (status, body, _) = Call::get("/admin/v1/billing")
        .tenant(tenant)
        .token(&sign(&claims("owner")))
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({
            "configured": true,
            "plan": "standard",
            "status": null,
            "current_period_end": null,
            "cancel_at_period_end": false,
            "subscription_open": false,
            "plans": ["pro"],
        })
    );
    let admin = sign(&claims("admin"));
    for call in [
        Call::get("/admin/v1/billing"),
        Call::post("/admin/v1/billing/checkout", json!({ "plan": "pro" })),
        Call::post("/admin/v1/billing/portal", json!({})),
    ] {
        let (status, body, _) = call.tenant(tenant).token(&admin).send(&s).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["code"], "insufficient_role");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn checkout_and_portal_open_stripe_for_the_store(db: PgPool) {
    let (s, _jwks) = setup(&db).await;
    let tenant = store_with_staff(&s).await;
    let owner = sign(&claims("owner"));

    let (status, body, _) = Call::post("/admin/v1/billing/portal", json!({}))
        .tenant(tenant)
        .token(&owner)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "no_billing_account");

    let (status, body, _) = Call::post("/admin/v1/billing/checkout", json!({ "plan": "gold" }))
        .tenant(tenant)
        .token(&owner)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "unknown_plan");

    let (status, body, _) = Call::post("/admin/v1/billing/checkout", json!({ "plan": "pro" }))
        .tenant(tenant)
        .token(&owner)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["url"].as_str().unwrap().starts_with("https://"),
        "{body}"
    );
    let customer: String = sqlx::query_scalar(
        "SELECT stripe_customer_id FROM platform.subscriptions WHERE tenant_id = $1",
    )
    .bind(tenant)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(customer.starts_with("cus_"));

    Call::post("/admin/v1/billing/checkout", json!({ "plan": "pro" }))
        .tenant(tenant)
        .token(&owner)
        .send(&s)
        .await;
    let customers: i64 =
        sqlx::query_scalar("SELECT count(*) FROM platform.subscriptions WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(customers, 1);

    let (status, body, _) = Call::post("/admin/v1/billing/portal", json!({}))
        .tenant(tenant)
        .token(&owner)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["url"].as_str().is_some());

    sqlx::query("UPDATE platform.subscriptions SET status = 'active' WHERE tenant_id = $1")
        .bind(tenant)
        .execute(&db)
        .await
        .unwrap();
    let (status, body, _) = Call::post("/admin/v1/billing/checkout", json!({ "plan": "pro" }))
        .tenant(tenant)
        .token(&owner)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "subscription_exists");

    let mut stale = claims("owner");
    stale["auth_time"] = json!(now() - 3600);
    let (status, body, _) = Call::post("/admin/v1/billing/portal", json!({}))
        .tenant(tenant)
        .token(&sign(&stale))
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "reauth_required");
}

#[sqlx::test(migrations = "../../migrations")]
async fn billing_without_configuration_says_so(db: PgPool) {
    let (s, _jwks) = setup(&db).await;
    let tenant = store_with_staff(&s).await;
    let off = off(&s);
    let owner = sign(&claims("owner"));
    let (status, body, _) = Call::get("/admin/v1/billing")
        .tenant(tenant)
        .token(&owner)
        .send(&off)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (body["configured"].clone(), body["plans"].clone()),
        (json!(false), json!([]))
    );
    for call in [
        Call::post("/admin/v1/billing/checkout", json!({ "plan": "pro" })),
        Call::post("/admin/v1/billing/portal", json!({})),
    ] {
        let (status, body, _) = call.tenant(tenant).token(&owner).send(&off).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "billing_not_configured");
    }
    let (status, _, _) = Call::post_raw("/webhooks/stripe/billing", "{}", "application/json")
        .header("stripe-signature", "t=1,v1=00")
        .send(&off)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn billing_webhook_verifies_and_stores_events(db: PgPool) {
    let (s, _jwks) = setup(&db).await;
    let raw = json!({
        "id": "evt_billing_1",
        "type": "customer.subscription.updated",
        "livemode": false,
        "data": { "object": { "id": "sub_1", "customer": "cus_1" } },
    })
    .to_string();
    let signer = |secret: &str| {
        commerce::payments::stripe::Stripe::new(
            &platform::config::StripeConfig {
                mode: platform::config::StripeMode::Simulator,
                api_url: "http://127.0.0.1:1".parse().unwrap(),
                secret_key: "sk_test_x".into(),
                publishable_key: None,
                webhook_secret: secret.into(),
            },
            reqwest::Client::new(),
        )
        .sign(raw.as_bytes(), Utc::now())
        .unwrap()
    };
    let (status, body, _) =
        Call::post_raw("/webhooks/stripe/billing", raw.clone(), "application/json")
            .header("stripe-signature", signer(STRIPE_WEBHOOK_SECRET))
            .send(&s)
            .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the Connect secret is not the billing one"
    );
    assert_eq!(body["code"], "invalid_signature");

    let (status, body, _) =
        Call::post_raw("/webhooks/stripe/billing", raw.clone(), "application/json")
            .header("stripe-signature", signer(BILLING_WEBHOOK_SECRET))
            .send(&s)
            .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "received": true, "new": true }));
    let kind: String = sqlx::query_scalar(
        "SELECT j.kind FROM queue.jobs j JOIN platform.provider_events e
           ON j.payload->>'id' = e.id::text WHERE e.event_id = 'evt_billing_1'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(kind, commerce::billing::EVENT_JOB);
}
