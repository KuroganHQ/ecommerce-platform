use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use commerce::cart::{self, NewLine};
use commerce::checkout::{self, ContactInput, Details};
use commerce::checkout_access::{self, ShopOrigin};
use commerce::consent::{self, ConsentPurpose, Subject};
use commerce::flows::{self, WatchInput};
use commerce::marketing::campaigns::{self, CampaignInput, EmailBlock, LocaleContent};
use commerce::marketing::subscribers;
use commerce::notifications::Template;
use commerce::orders;
use commerce::storefront::PublicUrls;
use commerce::tenancy::CheckoutMode;
use commerce::withdrawals::{self, LinkRequest};
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use testkit::storefront::{Shop, raw_order, shop};
use uuid::Uuid;

use crate::checkout_access::{At, alias, at, attest, switch};

const PLATFORM_PATHS: [&str; 9] = [
    "/o/",
    "/withdraw",
    "/review",
    "/newsletter",
    "/watch/",
    "/flows/",
    "/restore-cart",
    "/consent",
    "/_p/",
];

fn isolated(origin: &ShopOrigin) -> String {
    origin.as_str().replacen("://", "://checkout.", 1)
}

fn platform_origins(body: &str) -> BTreeSet<String> {
    body.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')'))
        .filter_map(|w| w.find("https://").map(|i| w[i..].replace("&amp;", "&")))
        .filter_map(|u| reqwest::Url::parse(&u).ok())
        .filter(|u| PLATFORM_PATHS.iter().any(|p| u.path().starts_with(p)))
        .map(|u| u.origin().ascii_serialization())
        .collect()
}

struct Mail {
    to: String,
    text: String,
    html: String,
    list_unsubscribe: Option<String>,
}

impl Mail {
    fn assert_on(&self, base: &str, what: &str) {
        let want = BTreeSet::from([base.to_owned()]);
        assert_eq!(platform_origins(&self.text), want, "{what}: text links");
        assert_eq!(platform_origins(&self.html), want, "{what}: HTML links");
        if let Some(h) = &self.list_unsubscribe {
            assert_eq!(platform_origins(h), want, "{what}: List-Unsubscribe");
        }
    }

    fn has(&self, url_prefix: &str) -> bool {
        self.text.contains(url_prefix) || self.html.contains(url_prefix)
    }
}

async fn mails(rt: &PgPool, tenant: Uuid, template: &str) -> Vec<Mail> {
    let mut tx = tenant_tx(rt, tenant).await.unwrap();
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT to_email, body_text, html, list_unsubscribe FROM email_messages
         WHERE template = $1 ORDER BY id",
    )
    .bind(template)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    rows.into_iter()
        .map(|(to, text, html, list_unsubscribe)| Mail {
            to,
            text,
            html,
            list_unsubscribe,
        })
        .collect()
}

async fn last_mail(rt: &PgPool, tenant: Uuid, template: &str) -> Mail {
    mails(rt, tenant, template).await.pop().unwrap()
}

async fn mail_to(rt: &PgPool, tenant: Uuid, template: &str, to: &str) -> Mail {
    let mut all = mails(rt, tenant, template).await;
    all.retain(|m| m.to == to);
    assert_eq!(all.len(), 1, "one {template} to {to}");
    all.pop().unwrap()
}

async fn order_on(rt: &PgPool, s: &Shop, origin: Option<&ShopOrigin>, status: &str) -> Uuid {
    let id = raw_order(rt, s, s.cz, "CZK", 12_900, 1, status).await;
    let mut tx = tenant_tx(rt, s.tenant).await.unwrap();
    sqlx::query("UPDATE orders SET shipping_method_snapshot = $2 WHERE id = $1")
        .bind(id)
        .bind(
            json!({"id": Uuid::nil(), "carrier": "personal_pickup", "name": "Osobní odběr",
                     "price_minor": 0}),
        )
        .execute(&mut *tx)
        .await
        .unwrap();
    if let Some(origin) = origin {
        sqlx::query(
            "UPDATE carts c SET checkout_origin = $2, checkout_generation = 1
             FROM orders o WHERE o.cart_id = c.id AND o.id = $1",
        )
        .bind(id)
        .bind(origin.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    id
}

async fn order_mail(rt: &PgPool, s: &Shop, order: Uuid, key: &str) -> Mail {
    let mut tx = tenant_tx(rt, s.tenant).await.unwrap();
    orders::mail::send(
        &mut tx,
        &PublicUrls::default(),
        order,
        Template::OrderDelivered,
        json!({}),
        key.into(),
        &[],
    )
    .await
    .unwrap();
    let (text, html): (String, String) =
        sqlx::query_as("SELECT body_text, html FROM email_messages WHERE idempotency_key = $1")
            .bind(key)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    Mail {
        to: String::new(),
        text,
        html,
        list_unsubscribe: None,
    }
}

async fn shop_with_alias(rt: &PgPool, slug: &str) -> (Shop, ShopOrigin) {
    let s = shop(rt, slug).await;
    let a = alias(rt, &s, &format!("{slug}-alias.localhost")).await;
    attest(rt, &a).await;
    switch(rt, &s, &a, CheckoutMode::SameOrigin).await;
    (s, a)
}

#[sqlx::test(migrations = "../../migrations")]
async fn order_mails_follow_the_origin_the_order_was_placed_on(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 2).await;
    let (s, a) = shop_with_alias(&rt, "lnkorder").await;
    let on_primary = order_on(&rt, &s, Some(&s.origin), "delivered").await;
    let on_alias = order_on(&rt, &s, Some(&a), "delivered").await;
    let unbound = order_on(&rt, &s, None, "delivered").await;

    let queued = order_mail(&rt, &s, on_primary, "lnk:primary:1").await;
    queued.assert_on(&isolated(&s.origin), "isolated primary order");
    assert!(queued.has("https://checkout.lnkorder.localhost/o/"));
    assert!(queued.has("https://checkout.lnkorder.localhost/withdraw"));
    order_mail(&rt, &s, unbound, "lnk:unbound:1")
        .await
        .assert_on(
            &isolated(&s.origin),
            "an order without an origin, isolated market",
        );
    let m = order_mail(&rt, &s, on_alias, "lnk:alias:1").await;
    m.assert_on(a.as_str(), "an order placed on the same-origin alias");
    assert!(m.has("https://lnkorder-alias.localhost/o/"));
    assert!(m.has("https://lnkorder-alias.localhost/withdraw"));

    attest(&rt, &s.origin).await;
    switch(&rt, &s, &s.origin, CheckoutMode::SameOrigin).await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    let still: (String, String) = sqlx::query_as(
        "SELECT body_text, html FROM email_messages WHERE idempotency_key = 'lnk:primary:1'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        still,
        (queued.text.clone(), queued.html.clone()),
        "mail queued before the switch keeps its checkout.<shop> links"
    );
    order_mail(&rt, &s, on_primary, "lnk:primary:2")
        .await
        .assert_on(
            s.origin.as_str(),
            "the same order after its origin switched",
        );
    order_mail(&rt, &s, unbound, "lnk:unbound:2")
        .await
        .assert_on(
            s.origin.as_str(),
            "an order without an origin, same-origin market",
        );
}

async fn request_mails(rt: &PgPool, s: &Shop, here: &At, order_number: i64, email: &str) {
    here.run(rt, async |tx, ctx| {
        withdrawals::request_link(
            tx,
            ctx,
            &LinkRequest {
                order_number: order_number.to_string(),
                email: "buyer@example.com".into(),
            },
        )
        .await
        .unwrap();
        subscribers::subscribe(tx, ctx, email, None, "form")
            .await
            .unwrap();
        flows::subscribe_watch(
            tx,
            ctx,
            &WatchInput {
                variant_id: s.variants[0],
                kind: "back_in_stock".into(),
                target_minor: None,
                email: email.into(),
            },
        )
        .await
        .unwrap();
    })
    .await;
}

async fn order_number(rt: &PgPool, s: &Shop, order: Uuid) -> i64 {
    let mut tx = tenant_tx(rt, s.tenant).await.unwrap();
    let n = sqlx::query_scalar("SELECT number FROM orders WHERE id = $1")
        .bind(order)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    n
}

#[sqlx::test(migrations = "../../migrations")]
async fn request_mails_link_to_the_requesting_origin(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 2).await;
    let (s, a) = shop_with_alias(&rt, "lnkreq").await;
    let order = order_on(&rt, &s, None, "delivered").await;
    let number = order_number(&rt, &s, order).await;
    let templates = ["withdrawal_link", "newsletter_confirm", "watch_confirm"];

    request_mails(&rt, &s, &at(&s, s.cz, &s.origin), number, "one@example.com").await;
    for t in templates {
        let m = last_mail(&rt, s.tenant, t).await;
        m.assert_on(&isolated(&s.origin), t);
    }
    assert!(
        last_mail(&rt, s.tenant, "withdrawal_link")
            .await
            .has("https://checkout.lnkreq.localhost/withdraw?t=")
    );
    assert!(
        last_mail(&rt, s.tenant, "newsletter_confirm")
            .await
            .has("https://checkout.lnkreq.localhost/newsletter/confirm?token=")
    );
    assert!(
        last_mail(&rt, s.tenant, "watch_confirm")
            .await
            .has("https://checkout.lnkreq.localhost/watch/confirm?token=")
    );

    request_mails(&rt, &s, &at(&s, s.cz, &a), number, "two@example.com").await;
    for t in templates {
        last_mail(&rt, s.tenant, t)
            .await
            .assert_on(a.as_str(), &format!("{t} requested on the alias"));
    }
    assert!(
        last_mail(&rt, s.tenant, "withdrawal_link")
            .await
            .has("https://lnkreq-alias.localhost/withdraw?t=")
    );
    assert!(
        last_mail(&rt, s.tenant, "newsletter_confirm")
            .await
            .has("https://lnkreq-alias.localhost/newsletter/confirm?token=")
    );
    assert!(
        last_mail(&rt, s.tenant, "watch_confirm")
            .await
            .has("https://lnkreq-alias.localhost/watch/confirm?token=")
    );
}

async fn typed_checkout_on(rt: &PgPool, s: &Shop, here: &At, email: &str) {
    here.run(rt, async |tx, ctx| {
        let (_, shop_token) = cart::create(tx, ctx).await.unwrap();
        let source = cart::find(tx, ctx, &shop_token).await.unwrap();
        let line = NewLine {
            variant_id: s.variants[0],
            quantity: 1,
        };
        cart::add_line(tx, ctx, &source, &line).await.unwrap();
        let k = checkout_access::start(tx, ctx, &shop_token, None)
            .await
            .unwrap();
        let contact = Details::Contact(ContactInput {
            email: email.into(),
            phone: None,
        });
        checkout::set_same_origin_details(tx, ctx, &k, &contact)
            .await
            .unwrap();
    })
    .await;
}

async fn isolated_cart(rt: &PgPool, s: &Shop, email: &str) {
    let mut tx = tenant_tx(rt, s.tenant).await.unwrap();
    let cart: Uuid = sqlx::query_scalar(
        "INSERT INTO carts (tenant_id, market_id, email, locale, currency, last_activity_at)
         VALUES ($1, $2, $3, 'cs', 'CZK', now()) RETURNING id",
    )
    .bind(s.tenant)
    .bind(s.cz)
    .bind(email)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO cart_lines (tenant_id, cart_id, variant_id, quantity) VALUES ($1, $2, $3, 1)",
    )
    .bind(s.tenant)
    .bind(cart)
    .bind(s.variants[0])
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn consent_to(rt: &PgPool, s: &Shop, email: &str, purpose: ConsentPurpose) {
    let mut tx = tenant_tx(rt, s.tenant).await.unwrap();
    consent::record_server(
        &mut tx,
        &Subject::Email(email.into()),
        purpose,
        true,
        consent::TEXT_VERSION,
        "checkout",
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn run_flows(rt: &PgPool, s: &Shop) {
    let mut tx = tenant_tx(rt, s.tenant).await.unwrap();
    sqlx::query("UPDATE carts SET last_activity_at = now() - interval '2 hours'")
        .execute(&mut *tx)
        .await
        .unwrap();
    flows::enroll_due(&mut tx, Utc::now()).await.unwrap();
    flows::execute_due(&mut tx, &PublicUrls::default(), Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn flow_mails_keep_the_origin_of_their_source(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 2).await;
    let (s, a) = shop_with_alias(&rt, "lnkflow").await;
    for email in ["iso@example.com", "alias@example.com"] {
        consent_to(&rt, &s, email, ConsentPurpose::EmailMarketing).await;
    }
    isolated_cart(&rt, &s, "iso@example.com").await;
    typed_checkout_on(&rt, &s, &at(&s, s.cz, &a), "alias@example.com").await;
    let delivered = order_on(&rt, &s, Some(&a), "delivered").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO shipments (tenant_id, order_id, carrier, status, delivered_at, created_by)
         VALUES ($1, $2, 'personal_pickup', 'delivered', $3, 'test')",
    )
    .bind(s.tenant)
    .bind(delivered)
    .bind(Utc::now() - Duration::days(8))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    consent_to(&rt, &s, "buyer@example.com", ConsentPurpose::ReviewInvites).await;
    run_flows(&rt, &s).await;

    let iso = mail_to(&rt, s.tenant, "abandoned_cart", "iso@example.com").await;
    let so = mail_to(&rt, s.tenant, "abandoned_cart", "alias@example.com").await;
    iso.assert_on(&isolated(&s.origin), "abandoned cart without an origin");
    assert!(iso.has("https://checkout.lnkflow.localhost/restore-cart?token="));
    assert!(iso.has("https://checkout.lnkflow.localhost/flows/unsubscribe?token="));
    assert!(
        iso.list_unsubscribe
            .as_deref()
            .unwrap()
            .contains("https://checkout.lnkflow.localhost/_p/flows/unsubscribe?token="),
        "isolated one-click keeps the old checkout-host URL"
    );
    so.assert_on(a.as_str(), "abandoned checkout started on the alias");
    assert!(so.has("https://lnkflow-alias.localhost/restore-cart?token="));
    assert!(so.has("https://lnkflow-alias.localhost/flows/unsubscribe?token="));
    let one_click = so.list_unsubscribe.as_deref().unwrap();
    assert!(
        one_click.contains("https://lnkflow-alias.localhost/_p/unsubscribe/"),
        "same-origin one-click is C11: {one_click}"
    );

    let review = last_mail(&rt, s.tenant, "review_invite").await;
    review.assert_on(a.as_str(), "review invite of an order placed on the alias");
    assert!(review.has("https://lnkflow-alias.localhost/review?token="));
}

#[sqlx::test(migrations = "../../migrations")]
async fn watch_alerts_keep_the_origin_the_watch_was_made_on(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 2).await;
    let (s, a) = shop_with_alias(&rt, "lnkwatch").await;
    for (here, email) in [
        (at(&s, s.cz, &s.origin), "iso@example.com"),
        (at(&s, s.cz, &a), "alias@example.com"),
    ] {
        here.run(&rt, async |tx, ctx| {
            flows::subscribe_watch(
                tx,
                ctx,
                &WatchInput {
                    variant_id: s.variants[0],
                    kind: "back_in_stock".into(),
                    target_minor: None,
                    email: email.into(),
                },
            )
            .await
            .unwrap();
        })
        .await;
    }
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    for m in mails(&rt, s.tenant, "watch_confirm").await {
        let at = m.text.find("token=").unwrap() + 6;
        flows::confirm_watch(&mut tx, &m.text[at..at + 64], Utc::now())
            .await
            .unwrap();
    }
    let event = json!({"variant_id": s.variants[0], "before": {"available": 0}, "after": {"available": 10}});
    flows::watch_event(
        &mut tx,
        &PublicUrls::default(),
        "inventory.changed",
        &event,
        Utc::now(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let iso = mail_to(&rt, s.tenant, "watch_alert", "iso@example.com").await;
    iso.assert_on(
        &isolated(&s.origin),
        "alert of a watch made on the isolated origin",
    );
    assert!(iso.has("https://checkout.lnkwatch.localhost/watch/unsubscribe?token="));
    let so = mail_to(&rt, s.tenant, "watch_alert", "alias@example.com").await;
    so.assert_on(a.as_str(), "alert of a watch made on the alias");
    assert!(so.has("https://lnkwatch-alias.localhost/watch/unsubscribe?token="));
}

async fn campaign(rt: &PgPool, s: &Shop) -> Mail {
    let mut tx = tenant_tx(rt, s.tenant).await.unwrap();
    let mut content = std::collections::BTreeMap::new();
    content.insert(
        "cs".to_owned(),
        LocaleContent {
            subject: "Novinky".into(),
            preheader: "Novinky".into(),
            blocks: vec![EmailBlock::Button {
                label: "Tričko".into(),
                href: "/c/tricka".into(),
            }],
        },
    );
    let c = campaigns::create(
        &mut tx,
        "staff",
        &CampaignInput {
            name: format!("C {}", Uuid::now_v7()),
            segment_id: None,
            content,
        },
    )
    .await
    .unwrap();
    campaigns::schedule(&mut tx, "staff", c.id, None, Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    campaigns::run_batch(rt, &PublicUrls::default(), s.tenant, c.id, Utc::now())
        .await
        .unwrap();
    last_mail(rt, s.tenant, "campaign").await
}

#[sqlx::test(migrations = "../../migrations")]
async fn campaign_links_and_one_click_follow_the_market_origin_mode(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 2).await;
    let s = shop(&rt, "lnkcmp").await;
    at(&s, s.cz, &s.origin)
        .run(&rt, async |tx, ctx| {
            subscribers::subscribe(tx, ctx, "anna@example.com", None, "form")
                .await
                .unwrap();
        })
        .await;
    let confirm = last_mail(&rt, s.tenant, "newsletter_confirm").await;
    let at_token = confirm.text.find("token=").unwrap() + 6;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    subscribers::confirm(
        &mut tx,
        &confirm.text[at_token..at_token + 64],
        Some(&[1; 32]),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let iso = campaign(&rt, &s).await;
    iso.assert_on(&isolated(&s.origin), "isolated campaign");
    assert!(iso.has("https://checkout.lnkcmp.localhost/_p/newsletter/click?t="));
    assert!(iso.has("https://checkout.lnkcmp.localhost/newsletter?t="));
    assert!(
        iso.list_unsubscribe
            .as_deref()
            .unwrap()
            .starts_with("https://checkout.lnkcmp.localhost/_p/newsletter/unsubscribe?t=")
    );

    attest(&rt, &s.origin).await;
    switch(&rt, &s, &s.origin, CheckoutMode::SameOrigin).await;
    let so = campaign(&rt, &s).await;
    so.assert_on(s.origin.as_str(), "same-origin campaign");
    assert!(so.has("https://lnkcmp.localhost/newsletter/click?t="));
    assert!(so.has("https://lnkcmp.localhost/newsletter?t="));
    let one_click = so.list_unsubscribe.as_deref().unwrap();
    assert!(
        one_click.starts_with("https://lnkcmp.localhost/_p/unsubscribe/"),
        "same-origin one-click is C11: {one_click}"
    );
}
