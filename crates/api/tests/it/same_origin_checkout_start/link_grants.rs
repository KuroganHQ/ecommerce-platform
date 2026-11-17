use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{Ctx, setup};
use crate::common::Call;
use commerce::tenancy::CheckoutMode;

const GRANT: &str = "x-link-grant";

impl Ctx {
    async fn exchange(
        &self,
        link: Value,
        previous: Option<&str>,
    ) -> (StatusCode, String, Option<String>) {
        let mut body = json!({ "link": link });
        if let Some(p) = previous {
            body["previous"] = json!(p);
        }
        let (status, text, res) = self
            .edge
            .call(Call::post("/storefront/v1/link-grants", body))
            .send_text(&self.s)
            .await;
        let grant = res
            .headers()
            .get(GRANT)
            .map(|v| v.to_str().unwrap().to_owned());
        (status, text, grant)
    }

    async fn order_by_grant(&self, id: Uuid, grant: Option<&str>) -> (StatusCode, String) {
        let uri = format!("/storefront/v1/orders/{id}");
        let mut call = self.edge.call(Call::get(&uri));
        if let Some(g) = grant {
            call = call.header(GRANT, g);
        }
        let (status, text, _) = call.send_text(&self.s).await;
        (status, text)
    }

    async fn request_withdrawal(&self, grant: &str, number: i64, email: &str) -> StatusCode {
        let body = json!({ "order_number": number.to_string(), "email": email });
        let (status, _, _) = self
            .edge
            .call(Call::post(
                "/storefront/v1/link-grants/withdrawal-request",
                body,
            ))
            .header(GRANT, grant)
            .send(&self.s)
            .await;
        status
    }

    async fn email_link(&self, order: Uuid) -> String {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        let token = commerce::orders::issue_token(&mut tx, order).await.unwrap();
        tx.commit().await.unwrap();
        token
    }

    async fn order(&self, status: &str) -> (Uuid, i64) {
        let id = testkit::storefront::raw_order(
            &self.runtime,
            &self.shop,
            self.shop.cz,
            "CZK",
            10_000,
            1,
            status,
        )
        .await;
        (
            id,
            self.scalar("SELECT number FROM orders WHERE id = $1", id)
                .await,
        )
    }

    async fn withdrawal_links(&self) -> i64 {
        self.scalar("SELECT count(*) FROM withdrawal_tokens WHERE $1", true)
            .await
    }

    async fn scalar<T, A>(&self, sql: &'static str, arg: A) -> T
    where
        T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
        A: for<'q> sqlx::Encode<'q, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send,
    {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        let v = sqlx::query_scalar(sql)
            .bind(arg)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        v
    }
}

fn is_capability(v: &Option<String>) -> bool {
    v.as_deref()
        .is_some_and(|g| g.len() == 64 && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[sqlx::test(migrations = "../../migrations")]
async fn order_link_exchanges_to_a_scoped_grant_and_no_body_carries_a_bearer(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let placed = c.placed(c.placeable().await, "").await;
    let a: Uuid = c.scalar("SELECT id FROM orders WHERE $1", true).await;
    let token = c.email_link(a).await;
    let (b, _) = c.order("pending").await;

    let (status, body, grant) = c
        .exchange(json!({ "kind": "order", "token": token }), None)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        is_capability(&grant),
        "the grant travels in a header: {grant:?}"
    );
    let grant = grant.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "resource": { "kind": "order", "id": a } })
    );

    let (status, view) = c.order_by_grant(a, Some(&grant)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let view: Value = serde_json::from_str(&view).unwrap();
    assert_eq!(view["number"], placed.number);
    assert_eq!(
        view["payment"]["can_pay"],
        json!(false),
        "a grant does not authorize paying"
    );
    for text in [&body, &view.to_string()] {
        assert!(
            !text.contains(&token) && !text.contains(&grant),
            "no bearer in {text}"
        );
    }

    assert_eq!(
        c.order_by_grant(b, Some(&grant)).await.0,
        StatusCode::NOT_FOUND,
        "wrong order"
    );
    assert_eq!(
        c.order_by_grant(a, None).await.0,
        StatusCode::NOT_FOUND,
        "no grant"
    );
    assert_eq!(
        c.request_withdrawal(&grant, 1, "buyer@example.com").await,
        StatusCode::NOT_FOUND,
        "an order read grant requests no withdrawal"
    );

    let (status, _, newer) = c
        .exchange(json!({ "kind": "order", "token": token }), Some(&grant))
        .await;
    assert_eq!(status, StatusCode::CREATED, "the email link stays reusable");
    assert_eq!(
        c.order_by_grant(a, Some(&grant)).await.0,
        StatusCode::NOT_FOUND,
        "the newer link revoked the prior grant"
    );
    assert_eq!(
        c.order_by_grant(a, newer.as_deref()).await.0,
        StatusCode::OK
    );

    let (status, body, grant) = c
        .exchange(json!({ "kind": "order", "token": "0".repeat(64) }), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(grant.is_none() && !body.contains(&"0".repeat(64)), "{body}");

    c.switch(CheckoutMode::Isolated).await;
    let (status, _, grant) = c
        .exchange(json!({ "kind": "order", "token": token }), None)
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "isolated origins exchange nothing"
    );
    assert!(grant.is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn anonymous_withdrawal_grant_reads_nothing_and_its_request_rolls_back(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let (id, number) = c.order("shipped").await;

    let (status, body, grant) = c
        .exchange(json!({ "kind": "withdrawal_request" }), None)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "resource": { "kind": "withdrawal_request" } })
    );
    let grant = grant.unwrap();
    assert_eq!(
        c.order_by_grant(id, Some(&grant)).await.0,
        StatusCode::NOT_FOUND,
        "an anonymous withdrawal-request grant reads no order"
    );

    assert_eq!(
        c.request_withdrawal(&grant, number, "not an email").await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(c.withdrawal_links().await, 0);
    assert_eq!(
        c.request_withdrawal(&grant, number, "buyer@example.com")
            .await,
        StatusCode::ACCEPTED,
        "the failed request rolled back the grant's completion"
    );
    assert_eq!(c.withdrawal_links().await, 1);
    assert_eq!(
        c.request_withdrawal(&grant, number, "buyer@example.com")
            .await,
        StatusCode::NOT_FOUND,
        "the completed request revoked the grant"
    );
    assert_eq!(c.withdrawal_links().await, 1);
}

impl Ctx {
    async fn on_order(
        &self,
        call: Call<'_>,
        cart: Option<&str>,
        grant: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut call = self.edge.call(call);
        if let Some(k) = cart {
            call = call.header("x-cart-token", k.to_owned());
        }
        if let Some(g) = grant {
            call = call.header(GRANT, g.to_owned());
        }
        let (status, body, _) = call.send(&self.s).await;
        (status, body)
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn clean_order_page_reads_by_grant_and_only_the_placing_checkout_pays(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let p = c.placed(c.placeable().await, "").await;
    let id: Uuid = p.order["order_id"].as_str().unwrap().parse().unwrap();
    let attempt = p.order["payment"]["attempt_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(p.order["confirmation_url"], format!("/o/{id}"));
    assert_eq!(
        p.order["payment"]["action"],
        json!({"type": "redirect", "url": format!("/_p/fake-pay/{attempt}?return=/o/{id}")}),
        "the provider returns to the clean order page"
    );
    let (_, _, grant) = c
        .exchange(
            json!({"kind": "order", "token": c.email_link(id).await}),
            None,
        )
        .await;
    let grant = grant.unwrap();
    let k = Some(p.current.as_str());
    let g = Some(grant.as_str());
    let order = format!("/storefront/v1/orders/{id}");
    let payment = format!("{order}/payment");
    let documents = format!("{order}/documents");

    let (status, view) = c.on_order(Call::get(&order), None, g).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["payment"]["can_pay"], false, "the grant only reads");
    let (status, view) = c.on_order(Call::get(&order), k, None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(
        view["payment"]["can_pay"], true,
        "the placing checkout pays"
    );
    for path in [&payment, &documents] {
        let (status, body) = c.on_order(Call::get(path), None, g).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
    for path in [&order, &payment, &documents] {
        let (status, _) = c.on_order(Call::get(path), None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} without authority");
    }

    let init = format!("{order}/payment-attempts/{attempt}/init");
    let fake_pay = format!("/storefront/v1/checkout/fake-pay/{attempt}");
    let failed = json!({"outcome": "failed"});
    for (call, what) in [
        (Call::post(&init, json!({})), "init"),
        (Call::post(&fake_pay, failed.clone()), "fake pay"),
        (Call::get(&fake_pay), "fake pay page"),
    ] {
        let (status, body) = c.on_order(call, None, g).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{what} by grant: {body}");
        assert_eq!(body["code"], "payment_not_allowed", "{what}");
    }
    let (status, page) = c.on_order(Call::post(&fake_pay, failed), k, None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["status"], "failed");

    let retry = format!("{order}/payment-attempts");
    let (status, body) = c.on_order(Call::post(&retry, json!({})), None, g).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "bearer-only retry: {body}");
    assert_eq!(body["code"], "payment_not_allowed");
    let (status, started) = c.on_order(Call::post(&retry, json!({})), k, None).await;
    assert_eq!(status, StatusCode::CREATED, "{started}");
    let second = started["attempt_id"].as_str().unwrap();
    assert_eq!(
        started["action"]["url"],
        format!("/_p/fake-pay/{second}?return=/o/{id}")
    );
}
