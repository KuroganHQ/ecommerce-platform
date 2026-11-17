//! A2 account half: codes the account origin issues and a same-origin checkout consumes.

use std::time::Duration;

use commerce::account_codes::{self, AccountEvidence, Purpose};
use commerce::capability;
use commerce::checkout_access::ShopOrigin;
use commerce::tenancy::CheckoutMode;
use platform::db::tenant_tx;
use sqlx::PgPool;
use testkit::storefront::{Shop, shop};
use uuid::Uuid;

use crate::checkout_access::{attest, switch};

const STATE: &str = "state_0123456789abcdef";

async fn same_origin(runtime: &PgPool, slug: &str) -> Shop {
    let shop = shop(runtime, slug).await;
    for origin in [&shop.origin, &shop.sk_origin] {
        attest(runtime, origin).await;
        switch(runtime, &shop, origin, CheckoutMode::SameOrigin).await;
    }
    shop
}

async fn customer(runtime: &PgPool, shop: &Shop, email: &str) -> Uuid {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let id = sqlx::query_scalar(
        "INSERT INTO customers (tenant_id, email, locale) VALUES ($1, $2, 'cs') RETURNING id",
    )
    .bind(shop.tenant)
    .bind(email)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn issue(runtime: &PgPool, shop: &Shop, purpose: Purpose, customer: Option<Uuid>) -> String {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let cb = account_codes::callback(&mut tx, shop.cz, purpose, STATE, shop.origin.as_str())
        .await
        .unwrap();
    let code = account_codes::issue(&mut tx, &cb, customer).await.unwrap();
    tx.commit().await.unwrap();
    code
}

async fn consume_at(
    runtime: &PgPool,
    tenant: Uuid,
    purpose: Purpose,
    state: &str,
    origin: &ShopOrigin,
    code: &str,
) -> Option<AccountEvidence> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let out = account_codes::consume(&mut tx, purpose, state, origin, code)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    out
}

async fn consume(
    runtime: &PgPool,
    shop: &Shop,
    purpose: Purpose,
    code: &str,
) -> Option<AccountEvidence> {
    consume_at(runtime, shop.tenant, purpose, STATE, &shop.origin, code).await
}

#[sqlx::test(migrations = "../../migrations")]
async fn code_is_single_use_and_stored_hashed(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = same_origin(&runtime, "once").await;
    let ana = customer(&runtime, &shop, "ana@example.test").await;
    let code = issue(&runtime, &shop, Purpose::Identify, Some(ana)).await;
    assert!(capability::well_formed(&code));

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let (code_hash, state_hash, origin): (Vec<u8>, Vec<u8>, String) =
        sqlx::query_as("SELECT code_hash, state_hash, callback_origin FROM account_codes")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(code_hash, capability::hash(&code));
    assert_eq!(state_hash, capability::hash(STATE));
    assert_eq!(origin, shop.origin.as_str());

    let first = consume(&runtime, &shop, Purpose::Identify, &code).await;
    assert_eq!(
        first,
        Some(AccountEvidence {
            customer_id: Some(ana)
        })
    );
    assert_eq!(
        consume(&runtime, &shop, Purpose::Identify, &code).await,
        None,
        "reused"
    );
    assert_eq!(
        consume(&runtime, &shop, Purpose::Identify, &"0".repeat(64)).await,
        None,
        "unknown"
    );
    assert_eq!(
        consume(&runtime, &shop, Purpose::Identify, "not-a-code").await,
        None
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn code_expires_after_60s(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = same_origin(&runtime, "expiry").await;
    let ana = customer(&runtime, &shop, "ana@example.test").await;
    let fresh = issue(&runtime, &shop, Purpose::Identify, Some(ana)).await;
    let stale = issue(&runtime, &shop, Purpose::Identify, Some(ana)).await;

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let ttl: Vec<f64> = sqlx::query_scalar(
        "SELECT extract(epoch FROM expires_at - created_at)::float8 FROM account_codes",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(ttl, [60.0, 60.0]);

    for (code, age) in [(&fresh, 59), (&stale, 61)] {
        sqlx::query(
            "UPDATE account_codes SET created_at = created_at - make_interval(secs => $2),
                                      expires_at = expires_at - make_interval(secs => $2)
             WHERE code_hash = $1",
        )
        .bind(capability::hash(code))
        .bind(f64::from(age))
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    assert_eq!(
        consume(&runtime, &shop, Purpose::Identify, &stale).await,
        None
    );
    assert_eq!(
        consume(&runtime, &shop, Purpose::Identify, &fresh).await,
        Some(AccountEvidence {
            customer_id: Some(ana)
        })
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn code_rejected_for_other_state_origin_purpose_or_tenant(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = same_origin(&runtime, "bound").await;
    let other = same_origin(&runtime, "bound-other").await;
    let ana = customer(&runtime, &shop, "ana@example.test").await;
    let code = issue(&runtime, &shop, Purpose::Identify, Some(ana)).await;

    let t = shop.tenant;
    for (why, out) in [
        (
            "purpose",
            consume_at(&runtime, t, Purpose::Placement, STATE, &shop.origin, &code).await,
        ),
        (
            "state",
            consume_at(
                &runtime,
                t,
                Purpose::Identify,
                "state_0123456789abcdeF",
                &shop.origin,
                &code,
            )
            .await,
        ),
        (
            "origin of another market",
            consume_at(
                &runtime,
                t,
                Purpose::Identify,
                STATE,
                &shop.sk_origin,
                &code,
            )
            .await,
        ),
        (
            "tenant",
            consume_at(
                &runtime,
                other.tenant,
                Purpose::Identify,
                STATE,
                &shop.origin,
                &code,
            )
            .await,
        ),
        (
            "tenant and its origin",
            consume_at(
                &runtime,
                other.tenant,
                Purpose::Identify,
                STATE,
                &other.origin,
                &code,
            )
            .await,
        ),
    ] {
        assert_eq!(out, None, "{why}");
    }
    assert_eq!(
        consume(&runtime, &shop, Purpose::Identify, &code).await,
        Some(AccountEvidence {
            customer_id: Some(ana)
        }),
        "the rejected attempts left the code usable"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn code_reports_the_account_it_was_issued_for(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = same_origin(&runtime, "who").await;
    let ana = customer(&runtime, &shop, "ana@example.test").await;
    let bob = customer(&runtime, &shop, "bob@example.test").await;
    let for_ana = issue(&runtime, &shop, Purpose::Placement, Some(ana)).await;
    let for_bob = issue(&runtime, &shop, Purpose::Placement, Some(bob)).await;
    assert_eq!(
        consume(&runtime, &shop, Purpose::Placement, &for_bob).await,
        Some(AccountEvidence {
            customer_id: Some(bob)
        })
    );
    assert_eq!(
        consume(&runtime, &shop, Purpose::Placement, &for_ana).await,
        Some(AccountEvidence {
            customer_id: Some(ana)
        })
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn placement_code_reports_no_account_when_signed_out(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = same_origin(&runtime, "nobody").await;
    let code = issue(&runtime, &shop, Purpose::Placement, None).await;
    assert_eq!(
        consume(&runtime, &shop, Purpose::Placement, &code).await,
        Some(AccountEvidence { customer_id: None })
    );

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let cb = account_codes::callback(
        &mut tx,
        shop.cz,
        Purpose::Identify,
        STATE,
        shop.origin.as_str(),
    )
    .await
    .unwrap();
    let err = account_codes::issue(&mut tx, &cb, None).await.unwrap_err();
    assert!(
        matches!(
            err,
            platform::Error::Unauthorized {
                code: "not_signed_in"
            }
        ),
        "{err:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn consume_belongs_to_the_callers_transaction(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = same_origin(&runtime, "race").await;
    let ana = customer(&runtime, &shop, "ana@example.test").await;
    let code = issue(&runtime, &shop, Purpose::Identify, Some(ana)).await;

    let mut aborted = tenant_tx(&runtime, shop.tenant).await.unwrap();
    assert!(
        account_codes::consume(&mut aborted, Purpose::Identify, STATE, &shop.origin, &code)
            .await
            .unwrap()
            .is_some()
    );
    aborted.rollback().await.unwrap();

    let mut first = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let won = account_codes::consume(&mut first, Purpose::Identify, STATE, &shop.origin, &code)
        .await
        .unwrap();
    assert_eq!(
        won,
        Some(AccountEvidence {
            customer_id: Some(ana)
        }),
        "a rolled-back consume frees the code"
    );
    let second = tokio::spawn({
        let (runtime, origin, code, tenant) = (
            runtime.clone(),
            shop.origin.clone(),
            code.clone(),
            shop.tenant,
        );
        async move { consume_at(&runtime, tenant, Purpose::Identify, STATE, &origin, &code).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !second.is_finished(),
        "the second consume waits for the first transaction"
    );
    first.commit().await.unwrap();
    assert_eq!(second.await.unwrap(), None, "exactly one consume wins");
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_consumes_have_one_winner(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 8).await;
    let shop = same_origin(&runtime, "burst").await;
    let code = issue(&runtime, &shop, Purpose::Placement, None).await;
    let tasks: Vec<_> = (0..6)
        .map(|_| {
            let (runtime, origin, code, tenant) = (
                runtime.clone(),
                shop.origin.clone(),
                code.clone(),
                shop.tenant,
            );
            tokio::spawn(async move {
                consume_at(&runtime, tenant, Purpose::Placement, STATE, &origin, &code).await
            })
        })
        .collect();
    let mut wins = 0;
    for t in tasks {
        wins += usize::from(t.await.unwrap().is_some());
    }
    assert_eq!(wins, 1);
}
