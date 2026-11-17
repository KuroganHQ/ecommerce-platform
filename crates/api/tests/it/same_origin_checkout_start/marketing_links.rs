use axum::http::StatusCode;
use commerce::capability;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{Ctx, setup};
use crate::common::Call;
use commerce::tenancy::CheckoutMode;

const GRANT: &str = "x-link-grant";
const EMAIL: &str = "jana@example.cz";

impl Ctx {
    async fn grant_for(&self, kind: &str, token: &str) -> String {
        let (status, text, res) = self
            .edge
            .call(Call::post(
                "/storefront/v1/link-grants",
                json!({ "link": { "kind": kind, "token": token } }),
            ))
            .send_text(&self.s)
            .await;
        assert_eq!(status, StatusCode::CREATED, "{kind}: {text}");
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({ "resource": { "kind": kind } })
        );
        assert!(!text.contains(token), "no bearer in the body");
        res.headers()[GRANT].to_str().unwrap().to_owned()
    }

    async fn exchange_status(&self, kind: &str, token: &str) -> StatusCode {
        self.edge
            .call(Call::post(
                "/storefront/v1/link-grants",
                json!({ "link": { "kind": kind, "token": token } }),
            ))
            .send_text(&self.s)
            .await
            .0
    }

    async fn by_grant(&self, call: Call<'_>, grant: &str) -> (StatusCode, String) {
        let (status, text, _) = self
            .edge
            .call(call)
            .header(GRANT, grant.to_owned())
            .send_text(&self.s)
            .await;
        (status, text)
    }

    async fn fixture(
        &self,
        q: sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
    ) {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        q.execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }

    async fn text(&self, sql: &'static str, hash: Vec<u8>) -> String {
        self.one(sql, hash).await
    }

    async fn one<T>(&self, sql: &'static str, hash: Vec<u8>) -> T
    where
        T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
    {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        let v = sqlx::query_scalar(sql)
            .bind(hash)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        v
    }

    async fn pending_subscriber(&self, email: &str) -> String {
        let link = capability::mint();
        self.fixture(
            sqlx::query(
                "INSERT INTO subscribers (tenant_id, email, status, locale, market_id,
                     confirm_token_hash, confirm_expires_at, text_version, source)
                 VALUES ($1, $2, 'pending', 'cs', $3, $4, now() + interval '1 day', 'v1', 'form')",
            )
            .bind(self.shop.tenant)
            .bind(email)
            .bind(self.shop.cz)
            .bind(&link.hash),
        )
        .await;
        link.token
    }

    async fn campaign_send(&self, key: &[u8]) -> String {
        let link = capability::mint();
        self.fixture(
            sqlx::query(
                "WITH s AS (
                     INSERT INTO subscribers (tenant_id, email, status, locale, market_id,
                         text_version, source, confirmed_at)
                     VALUES ($1, $2, 'subscribed', 'cs', $3, 'v1', 'form', now())
                     ON CONFLICT (tenant_id, email) DO UPDATE SET status = 'subscribed'
                     RETURNING id),
                 c AS (
                     INSERT INTO campaigns (tenant_id, name, content, link_key, created_by)
                     VALUES ($1, 'Jaro', '{}', $4, 'test') RETURNING id)
                 INSERT INTO campaign_sends (tenant_id, campaign_id, subscriber_id, status,
                     token_hash)
                 SELECT $1, c.id, s.id, 'sent', $5 FROM s, c",
            )
            .bind(self.shop.tenant)
            .bind(EMAIL)
            .bind(self.shop.cz)
            .bind(key)
            .bind(&link.hash),
        )
        .await;
        link.token
    }

    async fn pending_watch(&self) -> (String, String) {
        let (confirm, unsubscribe) = (capability::mint(), capability::mint());
        self.fixture(
            sqlx::query(
                "INSERT INTO flow_watches (tenant_id, market_id, variant_id, kind, email, locale,
                     confirm_hash, unsubscribe_hash, confirm_expires_at)
                 VALUES ($1, $2, $3, 'back_in_stock', $4, 'cs', $5, $6,
                     now() + interval '1 day')",
            )
            .bind(self.shop.tenant)
            .bind(self.shop.cz)
            .bind(self.shop.variants[0])
            .bind(EMAIL)
            .bind(&confirm.hash)
            .bind(&unsubscribe.hash),
        )
        .await;
        (confirm.token, unsubscribe.token)
    }

    async fn flow_opt_out(&self) -> String {
        let link = capability::mint();
        self.fixture(
            sqlx::query(
                "INSERT INTO flow_unsubscribe_tokens (tenant_id, token_hash, email)
                 VALUES ($1, $2, $3)",
            )
            .bind(self.shop.tenant)
            .bind(&link.hash)
            .bind(EMAIL),
        )
        .await;
        link.token
    }

    async fn subscriber_status(&self) -> String {
        self.text(
            "SELECT status FROM subscribers WHERE email = convert_from($1, 'UTF8')",
            EMAIL.as_bytes().to_vec(),
        )
        .await
    }

    async fn watch_status(&self, unsubscribe: &str) -> String {
        self.text(
            "SELECT status FROM flow_watches WHERE unsubscribe_hash = $1",
            capability::hash(unsubscribe),
        )
        .await
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn newsletter_grants_confirm_once_and_manage_preferences_without_a_bearer(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let link = c.pending_subscriber(EMAIL).await;
    let grant = c.grant_for("newsletter_confirmation", &link).await;

    let (status, body) = c
        .by_grant(
            Call::get("/storefront/v1/link-grants/newsletter-confirmation"),
            &grant,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "email": "j***@example.cz" })
    );
    assert_eq!(
        c.by_grant(
            Call::get("/storefront/v1/link-grants/newsletter-preferences"),
            &grant
        )
        .await
        .0,
        StatusCode::NOT_FOUND,
        "a confirmation grant reads no preferences"
    );
    let confirm = || {
        Call::post(
            "/storefront/v1/link-grants/newsletter-confirmation",
            json!({}),
        )
    };
    let (status, body) = c.by_grant(confirm(), &grant).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(c.subscriber_status().await, "subscribed");
    assert_eq!(
        c.by_grant(confirm(), &grant).await.0,
        StatusCode::NOT_FOUND,
        "single use"
    );
    assert_eq!(
        c.exchange_status("newsletter_confirmation", &link).await,
        StatusCode::NOT_FOUND,
        "a used confirmation link exchanges to nothing"
    );

    let send = c.campaign_send(&[7; 32]).await;
    let prefs = c.grant_for("newsletter_preferences", &send).await;
    assert_eq!(
        c.by_grant(confirm(), &prefs).await.0,
        StatusCode::NOT_FOUND,
        "a preferences grant confirms nothing"
    );
    let read = || Call::get("/storefront/v1/link-grants/newsletter-preferences");
    let (status, body) = c.by_grant(read(), &prefs).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "email": "j***@example.cz", "status": "subscribed" })
    );
    let (status, body) = c
        .by_grant(
            Call::post(
                "/storefront/v1/link-grants/newsletter-preferences/unsubscribe",
                json!({}),
            ),
            &prefs,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(c.subscriber_status().await, "unsubscribed");
    let (status, body) = c
        .by_grant(
            Call::post(
                "/storefront/v1/link-grants/newsletter-preferences/resubscribe",
                json!({}),
            ),
            &prefs,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(
        c.subscriber_status().await,
        "pending",
        "a new double opt-in"
    );
    assert!(!body.contains(EMAIL));
    assert_eq!(
        c.by_grant(read(), &prefs).await.0,
        StatusCode::OK,
        "the preferences grant outlives its actions"
    );

    c.switch(CheckoutMode::Isolated).await;
    assert_eq!(c.by_grant(read(), &prefs).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        c.exchange_status("newsletter_preferences", &send).await,
        StatusCode::CONFLICT
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn watch_and_flow_grants_act_once_on_their_own_link(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let (confirm, unsubscribe) = c.pending_watch().await;
    let opt_out = c.flow_opt_out().await;
    let post = |path: &'static str| Call::post(path, json!({}));
    const CONFIRM: &str = "/storefront/v1/link-grants/watch-confirmation";
    const UNWATCH: &str = "/storefront/v1/link-grants/watch-unsubscribe";
    const FLOW: &str = "/storefront/v1/link-grants/flow-unsubscribe";

    let g = c.grant_for("watch_confirmation", &confirm).await;
    assert_eq!(c.by_grant(post(UNWATCH), &g).await.0, StatusCode::NOT_FOUND);
    assert_eq!(c.watch_status(&unsubscribe).await, "pending");
    assert_eq!(c.by_grant(post(CONFIRM), &g).await.0, StatusCode::OK);
    assert_eq!(c.watch_status(&unsubscribe).await, "confirmed");
    assert_eq!(c.by_grant(post(CONFIRM), &g).await.0, StatusCode::NOT_FOUND);

    let g = c.grant_for("watch_unsubscribe", &unsubscribe).await;
    assert_eq!(c.by_grant(post(FLOW), &g).await.0, StatusCode::NOT_FOUND);
    assert_eq!(c.by_grant(post(UNWATCH), &g).await.0, StatusCode::OK);
    assert_eq!(c.watch_status(&unsubscribe).await, "unsubscribed");
    assert_eq!(c.by_grant(post(UNWATCH), &g).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        c.exchange_status("watch_unsubscribe", &unsubscribe).await,
        StatusCode::NOT_FOUND
    );

    let g = c.grant_for("flow_unsubscribe", &opt_out).await;
    assert_eq!(c.by_grant(post(FLOW), &g).await.0, StatusCode::OK);
    let used = c
        .text(
            "SELECT (used_at IS NOT NULL)::text FROM flow_unsubscribe_tokens WHERE token_hash = $1",
            capability::hash(&opt_out),
        )
        .await;
    assert_eq!(used, "true");
    assert_eq!(c.by_grant(post(FLOW), &g).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        c.exchange_status("flow_unsubscribe", &opt_out).await,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn restore_grant_single_use(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let shop_token = c.shop_cart().await;
    c.add(&shop_token, 1).await;
    let checkout = c.started(&shop_token, None).await;
    let checkout = c
        .rotated(
            &checkout,
            json!({ "contact": { "email": EMAIL, "phone": "+420777123456" } }),
        )
        .await;
    let abandoned: Uuid = c
        .one(
            "SELECT id FROM carts WHERE status = 'open' AND source_cart_id =
                 (SELECT id FROM carts WHERE shop_token_hash = $1)",
            capability::hash(&shop_token),
        )
        .await;
    let link = capability::mint();
    c.fixture(
        sqlx::query(
            "INSERT INTO flow_restore_tokens (tenant_id, token_hash, cart_id, expires_at)
             VALUES ($1, $2, $3, now() + interval '1 day')",
        )
        .bind(c.shop.tenant)
        .bind(&link.hash)
        .bind(abandoned),
    )
    .await;

    let grant = c.grant_for("restore_cart", &link.token).await;
    let restore = || Call::post("/storefront/v1/link-grants/restore-cart", json!({}));
    let (status, body) = c.by_grant(restore(), &grant).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body.as_object().unwrap().len(), 1, "{body}");
    let restored = body["cart_token"].as_str().unwrap().to_owned();
    assert!(capability::well_formed(&restored));
    assert!(restored != checkout && restored != shop_token);

    let (status, cart) = c.call(Call::get("/storefront/v1/cart"), &restored).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the restored cart is a shop cart: {cart}"
    );
    assert_eq!(cart["lines"].as_array().unwrap().len(), 2, "{cart}");
    assert_eq!(
        c.checkout(&restored).await.0,
        StatusCode::NOT_FOUND,
        "the restored cart is no checkout"
    );
    let pii: bool = c
        .one(
            "SELECT email IS NOT NULL OR customer_id IS NOT NULL OR phone IS NOT NULL
             FROM carts WHERE shop_token_hash = $1",
            capability::hash(&restored),
        )
        .await;
    assert!(!pii, "no PII is copied");
    assert_eq!(
        c.checkout(&checkout).await.0,
        StatusCode::OK,
        "the abandoned checkout stays its own"
    );

    assert_eq!(
        c.by_grant(restore(), &grant).await.0,
        StatusCode::NOT_FOUND,
        "the grant is spent"
    );
    assert_eq!(
        c.exchange_status("restore_cart", &link.token).await,
        StatusCode::NOT_FOUND,
        "the link is spent"
    );
}

fn encode(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Ctx {
    async fn click(&self, t: &str, u: &str, s: &str) -> (StatusCode, String) {
        let uri = format!(
            "/storefront/v1/newsletter/click?t={}&u={}&s={}",
            encode(t),
            encode(u),
            encode(s)
        );
        let (status, text, _) = self.edge.call(Call::get(&uri)).send_text(&self.s).await;
        (status, text)
    }

    async fn one_click(&self, call: Call<'_>) -> (StatusCode, String) {
        let (status, text, _) = self.edge.call(call).send_text(&self.s).await;
        (status, text)
    }
}

const ONE_CLICK: &str = "/storefront/v1/unsubscribe";

#[sqlx::test(migrations = "../../migrations")]
async fn signed_click_no_open_redirect(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let key = [9u8; 32];
    let send = c.campaign_send(&key).await;
    let target = "https://shop.localhost/p/jarni-kabat?utm=jaro";
    let sig = commerce::marketing::campaigns::sign(&key, &send, target).unwrap();
    let (status, body) = c.click(&send, target, &sig).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "url": target }),
        "only the signed target, nothing about the subscriber"
    );

    let other = c.campaign_send(&[3; 32]).await;
    for (what, t, u, s) in [
        (
            "another target",
            send.as_str(),
            "https://evil.example/",
            sig.as_str(),
        ),
        ("another recipient", other.as_str(), target, sig.as_str()),
        ("a truncated signature", send.as_str(), target, &sig[..10]),
    ] {
        let (status, body) = c.click(t, u, s).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{what}: {body}");
    }
    for unsafe_target in ["//evil.example/", "javascript:alert(1)", "/o/x"] {
        let s = commerce::marketing::campaigns::sign(&key, &send, unsafe_target).unwrap();
        let (status, body) = c.click(&send, unsafe_target, &s).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{unsafe_target}: {body}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn one_click_without_origin_or_cookie(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let send = c.campaign_send(&[7; 32]).await;
    let opt_out = c.flow_opt_out().await;

    let (status, body) = c
        .one_click(Call::post(ONE_CLICK, json!({ "token": send })))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(c.subscriber_status().await, "unsubscribed");
    assert!(!body.contains(EMAIL) && !body.contains("***"), "{body}");
    let (status, body) = c
        .one_click(Call::post(ONE_CLICK, json!({ "token": opt_out })))
        .await;
    assert_eq!(status, StatusCode::OK, "a flow opt-out link too: {body}");
    let (status, _) = c
        .one_click(Call::post(
            ONE_CLICK,
            json!({ "token": capability::mint().token }),
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn one_click_ignores_attacker_cookie(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let victim = c.campaign_send(&[7; 32]).await;
    let attacker_link = c.pending_subscriber("eva@example.cz").await;
    let attacker_grant = c.grant_for("newsletter_confirmation", &attacker_link).await;
    let attacker_cart = c.shop_cart().await;

    let (status, body) = c
        .one_click(
            Call::post(ONE_CLICK, json!({ "token": victim }))
                .header(GRANT, attacker_grant.clone())
                .header("x-cart-token", attacker_cart.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(c.subscriber_status().await, "unsubscribed");
    let (status, body) = c
        .by_grant(
            Call::get("/storefront/v1/link-grants/newsletter-confirmation"),
            &attacker_grant,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the planted grant is neither used nor revoked: {body}"
    );
    let (status, _) = c
        .one_click(Call::post(ONE_CLICK, json!({})).header(GRANT, attacker_grant.clone()))
        .await;
    assert!(
        status.is_client_error(),
        "a grant without the bearer does nothing"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn one_click_get_does_not_unsubscribe(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let send = c.campaign_send(&[7; 32]).await;
    let uri = format!("{ONE_CLICK}?token={send}");
    let (status, _) = c.one_click(Call::get(&uri)).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(c.subscriber_status().await, "subscribed");
}
