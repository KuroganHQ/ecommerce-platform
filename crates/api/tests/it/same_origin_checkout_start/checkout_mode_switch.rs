use tokio::task::JoinHandle;

use super::checkout_identity::checkout_cart;
use super::pii_rotation::{holds_a_capability, place_body};
use super::*;

type Answer = (StatusCode, String, Option<String>);

struct IsolatedOrder {
    cart: String,
    token: String,
    attempt: String,
}

struct Effects {
    unconsumed: [i64; 3],
    orders: i64,
    handoffs_unused: i64,
}

impl Ctx {
    async fn isolated_ready(&self, method: Uuid) -> (String, Value) {
        let cart = self.handed_off_via(&self.edge, self.shop.variants[0]).await;
        for step in [
            Call::put(
                "/storefront/v1/checkout/contact",
                json!({ "email": "jana@example.cz" }),
            ),
            Call::put(
                "/storefront/v1/checkout/addresses",
                json!({ "billing": { "name": "Jana", "street": "Dlouhá 12", "city": "Praha",
                                     "postal_code": "110 00", "country": "CZ" } }),
            ),
        ] {
            let (status, body) = self.call(step, &cart).await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        let view = self.ready(&cart, method).await;
        (cart, view)
    }

    async fn isolated_order(&self) -> IsolatedOrder {
        let (cart, view) = self.isolated_ready(self.placeable().await).await;
        let (status, text, _) = self.place(&cart, "isolated", &place_body(&view, "")).await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        let placed: Value = serde_json::from_str(&text).unwrap();
        IsolatedOrder {
            cart,
            token: placed["confirmation_url"]
                .as_str()
                .unwrap()
                .trim_start_matches("/o/")
                .to_owned(),
            attempt: placed["payment"]["attempt_id"].as_str().unwrap().to_owned(),
        }
    }

    async fn can_pay(&self, order: &IsolatedOrder) -> bool {
        let (status, body) = self
            .call(
                Call::get(&format!("/storefront/v1/orders/{}", order.token)),
                &order.cart,
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the order token still reads: {body}"
        );
        body["payment"]["can_pay"].as_bool().unwrap()
    }

    async fn fake_pay_page(&self, order: &IsolatedOrder) -> StatusCode {
        let uri = format!("/storefront/v1/checkout/fake-pay/{}", order.attempt);
        self.call(Call::get(&uri), &order.cart).await.0
    }

    fn spawn(&self, call: Call<'static>, cart: Option<String>) -> JoinHandle<Answer> {
        let s = self.s.clone();
        let mut call = self.edge.call(call);
        if let Some(cart) = cart {
            call = call.header("x-cart-token", cart);
        }
        tokio::spawn(async move {
            let (status, text, res) = call.send_text(&s).await;
            (status, text, replacement(&res))
        })
    }

    async fn blocked(&self, n: i64) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM pg_stat_activity
                 WHERE datname = current_database() AND cardinality(pg_blocking_pids(pid)) > 0",
            )
            .fetch_one(&self.db)
            .await
            .unwrap()
                < n
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{n} backends never queued"));
    }

    async fn shop_effects(&self) -> Effects {
        Effects {
            unconsumed: [
                self.count("SELECT count(*) FROM checkout_identities WHERE consumed_at IS NULL")
                    .await,
                self.count("SELECT count(*) FROM checkout_placements WHERE consumed_at IS NULL")
                    .await,
                self.count("SELECT count(*) FROM account_codes WHERE consumed_at IS NULL")
                    .await,
            ],
            orders: self.count("SELECT count(*) FROM orders").await,
            handoffs_unused: self
                .count("SELECT count(*) FROM checkout_handoffs WHERE used_at IS NULL")
                .await,
        }
    }

    /// A request holding the fence queues the admin switch to `mode`, `ops` queue behind the
    /// switch; the request then locks every cart of the shop, as a placement would, and
    /// commits. An op that locked a cart before the fence deadlocks here.
    async fn across_switch(
        &self,
        mode: &'static str,
        ops: Vec<(&str, Call<'static>, Option<String>)>,
    ) {
        let before = self.shop_effects().await;
        let mut request = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock_shared(platform.checkout_fence_key($1))")
            .bind(self.shop.tenant)
            .execute(&mut *request)
            .await
            .unwrap();
        let switch = tokio::spawn({
            let (s, tenant, origin) = (self.s.clone(), self.shop.tenant, self.edge.origin.clone());
            let token = sign(&claims(super::checkout_origins::OWNER));
            async move {
                let (status, body, _) = Call::post(
                    "/admin/v1/checkout-origins/mode",
                    json!({ "origin": origin, "mode": mode }),
                )
                .token(&token)
                .tenant(tenant)
                .send(&s)
                .await;
                (status, body)
            }
        });
        self.blocked(1).await;
        let n = i64::try_from(ops.len()).unwrap();
        let answers: Vec<_> = ops
            .into_iter()
            .map(|(what, call, cart)| (what, self.spawn(call, cart)))
            .collect();
        self.blocked(1 + n).await;
        sqlx::query("SELECT id FROM carts FOR UPDATE")
            .execute(&mut *request)
            .await
            .expect("no op holds a cart while it waits for the fence");
        request.commit().await.unwrap();
        let (status, body) = switch.await.unwrap();
        assert_eq!(
            (status, body["mode"].as_str()),
            (StatusCode::OK, Some(mode)),
            "{body}"
        );
        for (what, answer) in answers {
            let (status, text, k) = tokio::time::timeout(Duration::from_secs(10), answer)
                .await
                .unwrap_or_else(|_| panic!("{what} never finished"))
                .unwrap();
            assert!(status.is_client_error(), "{what}: {status} {text}");
            assert_eq!(k, None, "{what}: no capability");
            assert!(!holds_a_capability(&text), "{what}: {text}");
        }
        let after = self.shop_effects().await;
        assert_eq!(
            after.unconsumed, before.unconsumed,
            "no state or code consumed"
        );
        assert_eq!(after.orders, before.orders, "no order");
        assert_eq!(
            after.handoffs_unused, before.handoffs_unused,
            "no handoff spent"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn start_pii_identify_and_placement_paused_across_a_switch_to_isolated_fail(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let shop_token = c.shop_cart().await;
    let pii = checkout_cart(&c).await;
    let identify = checkout_cart(&c).await;
    let callback = checkout_cart(&c).await;
    let state = c.identify(&callback).await.state;
    let code = c.code(&ana, "identify", &state).await;
    let (place, view) = c.linked(method, &ana).await;
    let (confirm, confirm_view) = c.linked(method, &ana).await;
    let placement = c
        .placement_state(&confirm, "first", &confirm_view, "")
        .await;
    let placement_code = c.placement_code(Some(&ana), &placement).await;

    c.across_switch(
        "isolated",
        vec![
            (
                "start",
                Call::post("/storefront/v1/cart/checkout", json!({})),
                Some(shop_token),
            ),
            (
                "PII write",
                Call::post(
                    "/storefront/v1/checkout/details",
                    super::pii_rotation::contact("victim@example.cz"),
                ),
                Some(pii),
            ),
            (
                "identify start",
                Call::post("/storefront/v1/checkout/identify", json!({})),
                Some(identify),
            ),
            (
                "identify callback",
                Call::post(
                    "/storefront/v1/checkout/identify/callback",
                    json!({ "state": state, "idt": state, "code": code }),
                ),
                Some(callback),
            ),
            (
                "placement",
                Call::post("/storefront/v1/checkout/place-order", place_body(&view, ""))
                    .key("paused"),
                Some(place),
            ),
            (
                "placement confirmation",
                Call::post(
                    "/storefront/v1/checkout/placement/callback",
                    json!({ "state": placement, "code": placement_code }),
                ),
                Some(confirm),
            ),
        ],
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn stale_requests_paused_across_a_switch_to_same_origin_fail(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let callback = checkout_cart(&c).await;
    let state = c.identify(&callback).await.state;
    let code = c.code(&ana, "identify", &state).await;
    let (confirm, confirm_view) = c.linked(method, &ana).await;
    let placement = c
        .placement_state(&confirm, "first", &confirm_view, "")
        .await;
    let placement_code = c.placement_code(Some(&ana), &placement).await;
    c.switch(CheckoutMode::Isolated).await;

    let shop_token = c.shop_cart().await;
    let (status, body) = c
        .call(
            Call::post("/storefront/v1/cart/handoff", json!({})),
            &c.shop_cart().await,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let handoff = body["token"].as_str().unwrap().to_owned();
    let pii = c.handed_off_via(&c.edge, c.shop.variants[0]).await;
    let (place, view) = c.isolated_ready(method).await;

    c.across_switch(
        "same_origin",
        vec![
            (
                "handoff start",
                Call::post("/storefront/v1/cart/handoff", json!({})),
                Some(shop_token.clone()),
            ),
            (
                "handoff redemption",
                Call::post(
                    "/storefront/v1/checkout/handoff",
                    json!({ "token": handoff }),
                ),
                None,
            ),
            (
                "isolated PII write",
                Call::put(
                    "/storefront/v1/checkout/contact",
                    json!({ "email": "victim@example.cz" }),
                ),
                Some(pii),
            ),
            (
                "isolated placement",
                Call::post("/storefront/v1/checkout/place-order", place_body(&view, ""))
                    .key("paused"),
                Some(place),
            ),
            (
                "identify callback from two switches ago",
                Call::post(
                    "/storefront/v1/checkout/identify/callback",
                    json!({ "state": state, "idt": state, "code": code }),
                ),
                Some(callback),
            ),
            (
                "placement confirmation from two switches ago",
                Call::post(
                    "/storefront/v1/checkout/placement/callback",
                    json!({ "state": placement, "code": placement_code }),
                ),
                Some(confirm),
            ),
        ],
    )
    .await;
    let (status, cart) = c.call(Call::get("/storefront/v1/cart"), &shop_token).await;
    assert_eq!(
        (status, cart["item_count"].as_i64()),
        (StatusCode::OK, Some(1)),
        "the refused handoff left the shop cart and its capability intact"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn isolated_pay_checks_the_generation(db: PgPool) {
    let c = setup(db, CheckoutMode::Isolated).await;
    let order = c.isolated_order().await;
    assert!(c.can_pay(&order).await, "the placing browser pays");
    assert_eq!(c.fake_pay_page(&order).await, StatusCode::OK);

    c.switch(CheckoutMode::SameOrigin).await;
    c.switch(CheckoutMode::Isolated).await;
    assert!(
        !c.can_pay(&order).await,
        "a capability from before the round trip no longer pays"
    );
    assert_eq!(
        c.fake_pay_page(&order).await,
        StatusCode::FORBIDDEN,
        "nor opens the pay page"
    );
}
