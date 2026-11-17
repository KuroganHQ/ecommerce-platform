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
        let uri = format!("/storefront/v1/link-grants/orders/{id}");
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
