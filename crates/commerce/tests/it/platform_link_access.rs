use commerce::capability;
use commerce::checkout_access::{self, ShopOrigin};
use commerce::link_access::{self, Scope, Source};
use commerce::order_access::{self, Access, Credentials};
use commerce::orders;
use commerce::tenancy::CheckoutMode;
use platform::Error;
use platform::db::tenant_tx;
use sqlx::PgPool;
use testkit::storefront::{Shop, shop};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::checkout_access::{At, at, attest, blocked_by, pid, shop_cart, switch};

impl At {
    async fn exchange(
        &self,
        runtime: &PgPool,
        source: Source<'_>,
        previous: Option<&str>,
    ) -> Result<(String, Scope), Error> {
        self.run(runtime, async |tx, ctx| {
            link_access::exchange(tx, ctx, source, previous)
                .await
                .map(|g| (g.token, g.scope))
        })
        .await
    }

    async fn authorize(&self, runtime: &PgPool, grant: &str, scope: Scope) -> Result<(), Error> {
        self.run(runtime, async |tx, ctx| {
            link_access::authorize(tx, ctx, grant, scope).await
        })
        .await
    }

    async fn complete(&self, runtime: &PgPool, grant: &str, scope: Scope) -> Result<(), Error> {
        self.run(runtime, async |tx, ctx| {
            link_access::complete(tx, ctx, grant, scope).await
        })
        .await
    }
}

async fn same_origin(runtime: &PgPool, slug: &str) -> (Shop, At) {
    let shop = shop(runtime, slug).await;
    attest(runtime, &shop.origin).await;
    switch(runtime, &shop, &shop.origin, CheckoutMode::SameOrigin).await;
    let at = at(&shop, shop.cz, &shop.origin);
    (shop, at)
}

async fn order(runtime: &PgPool, shop: &Shop) -> (Uuid, String) {
    let id =
        testkit::storefront::raw_order(runtime, shop, shop.cz, "CZK", 10_000, 1, "pending").await;
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let token = orders::issue_token(&mut tx, id).await.unwrap();
    tx.commit().await.unwrap();
    (id, token)
}

async fn set_idle(runtime: &PgPool, tenant: Uuid, grant: &str, ago: &str) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(
        "UPDATE link_grants SET last_activity_at = now() - $2::interval WHERE token_hash = $1",
    )
    .bind(capability::hash(grant))
    .bind(ago)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn idle_since(runtime: &PgPool, tenant: Uuid, grant: &str) -> f64 {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_scalar(
        "SELECT extract(epoch FROM now() - last_activity_at)::float8
         FROM link_grants WHERE token_hash = $1",
    )
    .bind(capability::hash(grant))
    .fetch_one(&mut *tx)
    .await
    .unwrap()
}

fn not_found<T: std::fmt::Debug>(r: &Result<T, Error>) -> bool {
    matches!(r, Err(Error::NotFound))
}

#[sqlx::test(migrations = "../../migrations")]
async fn grant_is_scoped_to_its_resource_and_action(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (shop, at) = same_origin(&runtime, "lgscope").await;
    let (a, a_token) = order(&runtime, &shop).await;
    let (b, _) = order(&runtime, &shop).await;

    let (grant, scope) = at
        .exchange(&runtime, Source::OrderLink(&a_token), None)
        .await
        .unwrap();
    assert_eq!(
        scope,
        Scope::OrderRead(a),
        "the order link grants reading order A"
    );
    assert!(
        at.authorize(&runtime, &grant, Scope::OrderRead(a))
            .await
            .is_ok()
    );
    assert!(
        not_found(&at.authorize(&runtime, &grant, Scope::OrderRead(b)).await),
        "wrong resource"
    );
    assert!(
        not_found(
            &at.authorize(&runtime, &grant, Scope::WithdrawalRequest)
                .await
        ),
        "wrong action"
    );
    assert!(
        not_found(
            &at.complete(&runtime, &grant, Scope::WithdrawalRequest)
                .await
        ),
        "an order read grant completes no withdrawal request"
    );

    let (anonymous, scope) = at
        .exchange(&runtime, Source::WithdrawalForm, None)
        .await
        .unwrap();
    assert_eq!(scope, Scope::WithdrawalRequest);
    for id in [a, b] {
        assert!(
            not_found(
                &at.authorize(&runtime, &anonymous, Scope::OrderRead(id))
                    .await
            ),
            "an anonymous withdrawal-request grant reads no order"
        );
    }
    assert!(
        at.authorize(&runtime, &anonymous, Scope::WithdrawalRequest)
            .await
            .is_ok()
    );
    for forged in ["", "short", &"z".repeat(64), &a_token] {
        assert!(
            not_found(&at.authorize(&runtime, forged, Scope::OrderRead(a)).await),
            "{forged:?} is no grant"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn idle_30m_reads_refresh_invalid_reads_do_not_and_link_expiry_holds(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (shop, at) = same_origin(&runtime, "lgidle").await;
    let (a, token) = order(&runtime, &shop).await;
    let (grant, _) = at
        .exchange(&runtime, Source::OrderLink(&token), None)
        .await
        .unwrap();

    set_idle(&runtime, shop.tenant, &grant, "29 minutes").await;
    let _ = at
        .authorize(&runtime, &grant, Scope::WithdrawalRequest)
        .await;
    assert!(
        idle_since(&runtime, shop.tenant, &grant).await > 28.0 * 60.0,
        "a refused read does not refresh"
    );
    assert!(
        at.authorize(&runtime, &grant, Scope::OrderRead(a))
            .await
            .is_ok()
    );
    assert!(
        idle_since(&runtime, shop.tenant, &grant).await < 60.0,
        "a valid read refreshes the idle expiry"
    );

    set_idle(&runtime, shop.tenant, &grant, "31 minutes").await;
    assert!(
        not_found(&at.authorize(&runtime, &grant, Scope::OrderRead(a)).await),
        "30 minutes idle expires the grant"
    );

    let (fresh, _) = at
        .exchange(&runtime, Source::OrderLink(&token), None)
        .await
        .unwrap();
    assert!(
        at.authorize(&runtime, &fresh, Scope::OrderRead(a))
            .await
            .is_ok(),
        "the email link stays reusable and mints a new grant"
    );
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query(
        "UPDATE order_tokens SET expires_at = now() - interval '1 second' WHERE token_hash = $1",
    )
    .bind(capability::hash(&token))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        not_found(&at.exchange(&runtime, Source::OrderLink(&token), None).await),
        "an expired email link exchanges to nothing"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn newer_link_revokes_prior_grant_and_a_failed_exchange_keeps_it(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (shop, at) = same_origin(&runtime, "lgnewer").await;
    let (a, a_token) = order(&runtime, &shop).await;
    let (b, b_token) = order(&runtime, &shop).await;

    let (first, _) = at
        .exchange(&runtime, Source::OrderLink(&a_token), None)
        .await
        .unwrap();
    assert!(
        not_found(
            &at.exchange(&runtime, Source::OrderLink(&"0".repeat(64)), Some(&first))
                .await
        ),
        "an unknown link exchanges to nothing"
    );
    assert!(
        at.authorize(&runtime, &first, Scope::OrderRead(a))
            .await
            .is_ok(),
        "a failed exchange keeps the prior grant"
    );
    let (second, _) = at
        .exchange(&runtime, Source::OrderLink(&b_token), Some(&first))
        .await
        .unwrap();
    assert!(
        not_found(&at.authorize(&runtime, &first, Scope::OrderRead(a)).await),
        "the newer link revoked the prior grant"
    );
    assert!(
        at.authorize(&runtime, &second, Scope::OrderRead(b))
            .await
            .is_ok()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn read_queued_behind_replacement_fails(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (shop, at) = same_origin(&runtime, "lgrace").await;
    let (a, token) = order(&runtime, &shop).await;
    let (first, _) = at
        .exchange(&runtime, Source::OrderLink(&token), None)
        .await
        .unwrap();

    let (mut replacing, ctx) = at.tx(&runtime).await;
    let replaced = link_access::exchange(
        &mut replacing,
        &ctx,
        Source::OrderLink(&token),
        Some(&first),
    )
    .await
    .unwrap();
    let replacing_pid = pid(&mut replacing).await;
    let (pid_tx, pid_rx) = oneshot::channel();
    let reader = tokio::spawn({
        let (runtime, at, first) = (runtime.clone(), at.clone(), first.clone());
        async move {
            let (mut tx, ctx) = at.tx(&runtime).await;
            pid_tx.send(pid(&mut tx).await).unwrap();
            let r = link_access::authorize(&mut tx, &ctx, &first, Scope::OrderRead(a)).await;
            tx.commit().await.unwrap();
            r
        }
    });
    blocked_by(&db, pid_rx.await.unwrap(), replacing_pid).await;
    replacing.commit().await.unwrap();

    let read = reader.await.unwrap();
    assert!(
        not_found(&read),
        "a read queued behind the replacement fails: {read:?}"
    );
    assert!(
        at.authorize(&runtime, &replaced.token, Scope::OrderRead(a))
            .await
            .is_ok()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_exchanges_both_revoke_the_cookies_grant(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (shop, at) = same_origin(&runtime, "lgtwice").await;
    let (a, token) = order(&runtime, &shop).await;
    let (held, _) = at
        .exchange(&runtime, Source::OrderLink(&token), None)
        .await
        .unwrap();

    let (mut first_tx, ctx) = at.tx(&runtime).await;
    let first = link_access::exchange(&mut first_tx, &ctx, Source::OrderLink(&token), Some(&held))
        .await
        .unwrap();
    let first_pid = pid(&mut first_tx).await;
    let (pid_tx, pid_rx) = oneshot::channel();
    let second = tokio::spawn({
        let (runtime, at, token, held) = (runtime.clone(), at.clone(), token.clone(), held.clone());
        async move {
            let (mut tx, ctx) = at.tx(&runtime).await;
            pid_tx.send(pid(&mut tx).await).unwrap();
            let r = link_access::exchange(&mut tx, &ctx, Source::OrderLink(&token), Some(&held))
                .await
                .map(|g| g.token);
            tx.commit().await.unwrap();
            r
        }
    });
    blocked_by(&db, pid_rx.await.unwrap(), first_pid).await;
    first_tx.commit().await.unwrap();
    let second = second
        .await
        .unwrap()
        .expect("the queued exchange succeeds too");

    assert!(
        not_found(&at.authorize(&runtime, &held, Scope::OrderRead(a)).await),
        "the grant the cookie held authorizes nothing"
    );
    assert_ne!(
        first.token, second,
        "each exchange answers with its own grant"
    );
    for (what, grant) in [("first", &first.token), ("second", &second)] {
        assert_ne!(grant, &held, "{what} is not the revoked grant");
        assert!(
            at.authorize(&runtime, grant, Scope::OrderRead(a))
                .await
                .is_ok(),
            "{what} exchange's own grant reads"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn single_use_action_replay_denied_preview_does_not_consume(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (_, at) = same_origin(&runtime, "lgonce").await;
    let (grant, _) = at
        .exchange(&runtime, Source::WithdrawalForm, None)
        .await
        .unwrap();

    for _ in 0..2 {
        assert!(
            at.authorize(&runtime, &grant, Scope::WithdrawalRequest)
                .await
                .is_ok(),
            "a preview does not consume the action"
        );
    }
    {
        let (mut tx, ctx) = at.tx(&runtime).await;
        link_access::complete(&mut tx, &ctx, &grant, Scope::WithdrawalRequest)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
    }
    assert!(
        at.authorize(&runtime, &grant, Scope::WithdrawalRequest)
            .await
            .is_ok(),
        "a completion rolled back with its transaction leaves the grant"
    );
    assert!(
        at.complete(&runtime, &grant, Scope::WithdrawalRequest)
            .await
            .is_ok()
    );
    assert!(
        not_found(
            &at.complete(&runtime, &grant, Scope::WithdrawalRequest)
                .await
        ),
        "replay of a completed action"
    );
    assert!(
        not_found(
            &at.authorize(&runtime, &grant, Scope::WithdrawalRequest)
                .await
        ),
        "completion revoked the grant"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn grant_denied_on_alias_other_tenant_and_after_mode_switch(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (shop, at) = same_origin(&runtime, "lgalias").await;
    let (_, other_at) = same_origin(&runtime, "lgalias-other").await;
    let (a, token) = order(&runtime, &shop).await;
    let (grant, _) = at
        .exchange(&runtime, Source::OrderLink(&token), None)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO platform.domains (hostname, tenant_id, market_id, verified_at)
         VALUES ('lgalias-alias.localhost', $1, $2, now())",
    )
    .bind(shop.tenant)
    .bind(shop.cz)
    .execute(&runtime)
    .await
    .unwrap();
    let alias_origin = ShopOrigin::parse("https://lgalias-alias.localhost").unwrap();
    attest(&runtime, &alias_origin).await;
    switch(&runtime, &shop, &alias_origin, CheckoutMode::SameOrigin).await;
    let alias = crate::checkout_access::at(&shop, shop.cz, &alias_origin);
    let (grant, _) = at
        .exchange(&runtime, Source::OrderLink(&token), Some(&grant))
        .await
        .unwrap();
    assert!(
        not_found(&alias.authorize(&runtime, &grant, Scope::OrderRead(a)).await),
        "an alias of the same shop is another origin"
    );
    assert!(
        not_found(
            &other_at
                .authorize(&runtime, &grant, Scope::OrderRead(a))
                .await
        ),
        "another tenant"
    );
    assert!(
        not_found(
            &other_at
                .exchange(&runtime, Source::OrderLink(&token), None)
                .await
        ),
        "another tenant cannot exchange this shop's link"
    );
    assert!(
        at.authorize(&runtime, &grant, Scope::OrderRead(a))
            .await
            .is_ok()
    );

    switch(&runtime, &shop, &shop.origin, CheckoutMode::Isolated).await;
    assert!(
        not_found(&at.authorize(&runtime, &grant, Scope::OrderRead(a)).await),
        "a mode switch ends every grant of the shop"
    );
    let refused = at.exchange(&runtime, Source::OrderLink(&token), None).await;
    assert!(
        matches!(&refused, Err(Error::Conflict { code, .. }) if *code == "checkout_mode_changed"),
        "isolated origins exchange nothing: {refused:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn grant_ends_when_its_link_is_erased(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (shop, at) = same_origin(&runtime, "lgerase").await;
    let (a, token) = order(&runtime, &shop).await;
    let (grant, _) = at
        .exchange(&runtime, Source::OrderLink(&token), None)
        .await
        .unwrap();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("DELETE FROM order_tokens WHERE order_id = $1")
        .bind(a)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        not_found(&at.authorize(&runtime, &grant, Scope::OrderRead(a)).await),
        "a grant ends with the email link it came from"
    );
}

async fn checkout_order(runtime: &PgPool, at: &At, shop: &Shop) -> (Uuid, String, String) {
    let shop_token = shop_cart(runtime, at, shop).await;
    let capability = at
        .run(runtime, async |tx, ctx| {
            checkout_access::start(tx, ctx, &shop_token, None).await
        })
        .await
        .unwrap();
    let order =
        testkit::storefront::raw_order(runtime, shop, shop.cz, "CZK", 10_000, 1, "pending").await;
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    sqlx::query(
        "WITH c AS (UPDATE carts SET status = 'converted' WHERE checkout_token_hash = $1
                    RETURNING id)
         UPDATE orders SET cart_id = (SELECT id FROM c) WHERE id = $2",
    )
    .bind(capability::hash(&capability))
    .bind(order)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (order, capability, shop_token)
}

impl At {
    async fn access(
        &self,
        runtime: &PgPool,
        order: Uuid,
        checkout: Option<&str>,
        grant: Option<&str>,
    ) -> Result<Access, Error> {
        self.run(runtime, async |tx, ctx| {
            order_access::authorize(tx, ctx, order, Credentials { checkout, grant }).await
        })
        .await
    }

    async fn may_pay(&self, runtime: &PgPool, order: Uuid, checkout: &str) -> bool {
        self.run(runtime, async |tx, ctx| {
            order_access::may_pay(tx, ctx, order, Some(checkout), None).await
        })
        .await
        .unwrap()
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn only_the_placing_checkout_pays_and_a_grant_only_reads(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (shop, at) = same_origin(&runtime, "oapay").await;
    let (a, k, _) = checkout_order(&runtime, &at, &shop).await;
    let (b, b_link) = order(&runtime, &shop).await;
    let a_link = orders_token(&runtime, &shop, a).await;
    let (grant, _) = at
        .exchange(&runtime, Source::OrderLink(&a_link), None)
        .await
        .unwrap();
    let (b_grant, _) = at
        .exchange(&runtime, Source::OrderLink(&b_link), None)
        .await
        .unwrap();

    assert_eq!(
        at.access(&runtime, a, Some(&k), None).await.unwrap(),
        Access::Pay
    );
    assert!(
        at.may_pay(&runtime, a, &k).await,
        "the placing checkout pays"
    );
    assert_eq!(
        at.access(&runtime, a, Some(&k), Some(&grant))
            .await
            .unwrap(),
        Access::Pay
    );
    assert_eq!(
        at.access(&runtime, a, None, Some(&grant)).await.unwrap(),
        Access::Read,
        "the grant alone only reads"
    );
    assert_eq!(
        at.access(&runtime, b, Some(&k), Some(&b_grant))
            .await
            .unwrap(),
        Access::Read,
        "another order's grant reads that order, the checkout does not pay it"
    );
    assert!(!at.may_pay(&runtime, b, &k).await);
    assert!(not_found(
        &at.access(&runtime, b, Some(&k), Some(&grant)).await
    ));
    assert!(not_found(&at.access(&runtime, a, None, None).await));
    assert!(not_found(
        &at.access(&runtime, a, Some(&grant), Some(&k)).await
    ));

    switch(&runtime, &shop, &shop.origin, CheckoutMode::Isolated).await;
    switch(&runtime, &shop, &shop.origin, CheckoutMode::SameOrigin).await;
    assert!(
        not_found(&at.access(&runtime, a, Some(&k), None).await),
        "a capability of an earlier generation reads nothing"
    );
    assert!(!at.may_pay(&runtime, a, &k).await, "nor pays");
}

async fn queued_behind_a_new_start(
    db: &PgPool,
    runtime: &PgPool,
    at: &At,
    shop: &Shop,
    pay: bool,
) -> Result<bool, Error> {
    let (a, k, shop_token) = checkout_order(runtime, at, shop).await;
    assert_eq!(
        at.access(runtime, a, Some(&k), None).await.unwrap(),
        Access::Pay
    );
    let (mut starting, ctx) = at.tx(runtime).await;
    checkout_access::start(&mut starting, &ctx, &shop_token, Some(&k))
        .await
        .unwrap();
    let starting_pid = pid(&mut starting).await;
    let (waiter_pid, waiter_pid_rx) = oneshot::channel();
    let waiter = tokio::spawn({
        let (runtime, at) = (runtime.clone(), at.clone());
        async move {
            let (mut tx, ctx) = at.tx(&runtime).await;
            waiter_pid.send(pid(&mut tx).await).unwrap();
            let r = if pay {
                order_access::may_pay(&mut tx, &ctx, a, Some(&k), None).await
            } else {
                let creds = Credentials {
                    checkout: Some(&k),
                    grant: None,
                };
                order_access::authorize(&mut tx, &ctx, a, creds)
                    .await
                    .map(|access| access == Access::Pay)
            };
            tx.commit().await.unwrap();
            r
        }
    });
    blocked_by(db, waiter_pid_rx.await.unwrap(), starting_pid).await;
    starting.commit().await.unwrap();
    waiter.await.unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn order_read_or_retry_after_checkout_rotation_denied(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (shop, at) = same_origin(&runtime, "oarace").await;

    let read = queued_behind_a_new_start(&db, &runtime, &at, &shop, false).await;
    assert!(
        not_found(&read),
        "a read queued behind the new checkout start fails: {read:?}"
    );
    let pay = queued_behind_a_new_start(&db, &runtime, &at, &shop, true).await;
    assert!(
        matches!(pay, Ok(false)),
        "a retry queued behind it may not pay: {pay:?}"
    );
}

async fn orders_token(runtime: &PgPool, shop: &Shop, order: Uuid) -> String {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let token = orders::issue_token(&mut tx, order).await.unwrap();
    tx.commit().await.unwrap();
    token
}
