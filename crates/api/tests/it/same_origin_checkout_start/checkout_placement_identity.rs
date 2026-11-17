use commerce::checkout_identity;

use super::checkout_identity::checkout_cart;
use super::pii_rotation::{addresses, contact, holds_a_capability, place_body};
use super::*;

const UNKNOWN_STATE: &str = "unknownstate_0123456789abcdef0123456789abcdef0123456789abcdef0";

impl Ctx {
    async fn linked(&self, method: Uuid, session: &str) -> (String, Value) {
        let cart = checkout_cart(self).await;
        let s = self.identify(&cart).await.state;
        let code = self.code(session, "identify", &s).await;
        let k = self.identified(&cart, &s, &code).await;
        let k = self.rotated(&k, contact("buyer@example.cz")).await;
        let k = self.rotated(&k, addresses("Buyer")).await;
        let view = self.ready(&k, method).await;
        (k, view)
    }

    async fn placement_state(&self, cart: &str, key: &str, view: &Value, notes: &str) -> String {
        let (status, text, res) = self.place(cart, key, &place_body(view, notes)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{text}");
        assert_eq!(replacement(&res), None, "no capability before the order");
        let body: Value = serde_json::from_str(&text).unwrap();
        body["state"].as_str().unwrap().to_owned()
    }

    async fn placement_code(&self, session: Option<&str>, state: &str) -> String {
        let mut call = self.edge.call(Call::post(
            "/storefront/v1/customer/account-codes",
            json!({ "purpose": "placement", "state": state, "origin": self.edge.origin }),
        ));
        if let Some(session) = session {
            call = call.header("x-customer-session", session.to_owned());
        }
        let (status, body, _) = call.send(&self.s).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["code"].as_str().unwrap().to_owned()
    }

    async fn confirm_as(
        &self,
        edge: &Edge,
        cart: &str,
        body: Value,
    ) -> (StatusCode, String, Option<String>) {
        let (status, text, res) = edge
            .call(Call::post(
                "/storefront/v1/checkout/placement/callback",
                body,
            ))
            .header("x-cart-token", cart.to_owned())
            .send_text(&self.s)
            .await;
        (status, text, replacement(&res))
    }

    async fn confirm(&self, cart: &str, state: &str, code: &str) -> (StatusCode, Value) {
        let (status, text, _) = self
            .confirm_as(&self.edge, cart, json!({ "state": state, "code": code }))
            .await;
        (status, serde_json::from_str(&text).unwrap_or_default())
    }

    async fn orders(&self) -> i64 {
        self.count("SELECT count(*) FROM orders").await
    }

    async fn unconsumed_placement(&self) -> (i64, i64) {
        (
            self.count("SELECT count(*) FROM checkout_placements WHERE consumed_at IS NULL")
                .await,
            self.count(
                "SELECT count(*) FROM account_codes
                 WHERE purpose = 'placement' AND consumed_at IS NULL",
            )
            .await,
        )
    }

    async fn assert_aborted(&self, edge: &Edge, cart: &str, body: Value, what: &str) {
        let orders = self.orders().await;
        let (status, text, k) = self.confirm_as(edge, cart, body).await;
        assert!(status.is_client_error(), "{what}: {status} {text}");
        assert_eq!(k, None, "{what}: no capability");
        assert!(!holds_a_capability(&text), "{what}: {text}");
        assert_eq!(self.orders().await, orders, "{what}: no order");
    }

    async fn context(&self, tx: &mut platform::db::TenantTx) -> storefront::Context {
        let mut ctx =
            storefront::context(tx, &PublicUrls::default(), self.shop.cz, None, Utc::now())
                .await
                .unwrap();
        ctx.origin = Some(self.shop.origin.clone());
        ctx
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn same_account_attributes_none_or_other_places_as_guest(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, ana_id) = c.signed_in("ana@example.cz").await;
    let (bob, _) = c.signed_in("bob@example.cz").await;

    let k = c
        .rotated(&checkout_cart(&c).await, contact("plain@example.cz"))
        .await;
    let k = c.rotated(&k, addresses("Plain")).await;
    let view = c.ready(&k, method).await;
    let (status, text, _) = c.place(&k, "unlinked", &place_body(&view, "")).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an unlinked checkout places at once: {text}"
    );

    for (who, session, guest) in [
        ("same", Some(ana.as_str()), false),
        ("none", None, true),
        ("other", Some(bob.as_str()), true),
    ] {
        let (cart, view) = c.linked(method, &ana).await;
        let state = c.placement_state(&cart, who, &view, "").await;
        let code = c.placement_code(session, &state).await;
        let (status, body) = c.confirm(&cart, &state, &code).await;
        assert_eq!(status, StatusCode::CREATED, "{who}: {body}");
        assert_eq!(body["as_guest"], guest, "{who}");
        let order: Uuid = body["order_id"].as_str().unwrap().parse().unwrap();
        let customer: Option<Uuid> = c
            .read("SELECT customer_id FROM orders WHERE id = $1", order)
            .await;
        assert_eq!(customer, (!guest).then_some(ana_id), "{who}");

        let (status, receipt) = c
            .call(Call::get("/storefront/v1/checkout/order"), &cart)
            .await;
        assert_eq!(status, StatusCode::OK, "{who}: {receipt}");
        assert_eq!(receipt["order_id"], body["order_id"], "{who}");
        assert_eq!(receipt["placed_as_guest"], guest, "{who}");
        assert_eq!(receipt["email"], "buyer@example.cz", "{who}");
    }
    assert_eq!(
        c.read::<i64>("SELECT count(*) FROM orders WHERE customer_id = $1", ana_id)
            .await,
        1,
        "only the vouched placement reached the expected account"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn wrong_purpose_origin_or_state_aborts_and_replay_finds_nothing(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let (cart, view) = c.linked(method, &ana).await;
    let state = c.placement_state(&cart, "first", &view, "").await;
    let (other_cart, other_view) = c.linked(method, &ana).await;
    let other_state = c
        .placement_state(&other_cart, "other", &other_view, "")
        .await;
    let identify_state = c.identify(&cart).await.state;
    let identify_code = c.code(&ana, "identify", &identify_state).await;
    let code = c.placement_code(Some(&ana), &state).await;
    let other_code = c.placement_code(Some(&ana), &other_state).await;

    for (body, what) in [
        (
            json!({ "state": state, "code": identify_code }),
            "an identify code",
        ),
        (
            json!({ "state": identify_state, "code": identify_code }),
            "an identify state and code",
        ),
        (
            json!({ "state": state, "code": other_code }),
            "another state's code",
        ),
        (
            json!({ "state": other_state, "code": other_code }),
            "another checkout's state",
        ),
        (
            json!({ "state": UNKNOWN_STATE, "code": code }),
            "an unknown state",
        ),
        (json!({ "code": code }), "no state"),
        (json!({ "state": state }), "no code"),
        (json!({ "state": "", "code": "" }), "empty"),
        (
            json!({ "state": state, "code": code, "notes": "changed" }),
            "a note at the callback",
        ),
    ] {
        c.assert_aborted(&c.edge, &cart, body, what).await;
    }
    let foreign = Edge {
        origin: "https://elsewhere.example.cz".into(),
        ..c.edge.clone()
    };
    c.assert_aborted(
        &foreign,
        &cart,
        json!({ "state": state, "code": code }),
        "another origin",
    )
    .await;
    c.assert_aborted(
        &c.edge,
        &other_cart,
        json!({ "state": state, "code": code }),
        "another checkout's capability",
    )
    .await;
    assert_eq!(c.unconsumed_placement().await, (2, 2), "nothing consumed");

    let (status, body) = c.confirm(&cart, &state, &code).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    c.assert_aborted(
        &c.edge,
        &cart,
        json!({ "state": state, "code": code }),
        "a replay",
    )
    .await;
    assert_eq!(c.orders().await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_state_or_code_aborts(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, ana_id) = c.signed_in("ana@example.cz").await;
    let (cart, view) = c.linked(method, &ana).await;
    let state = c.placement_state(&cart, "ttl", &view, "").await;
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let (ttl, bound): (f64, bool) = sqlx::query_as(
        "SELECT extract(epoch FROM expires_at - created_at)::float8,
                capability_hash = sha256($1::text::bytea) AND origin = $2 AND generation > 0
                AND expected_customer_id = $3 AND input->'version' = $4
         FROM checkout_placements",
    )
    .bind(&cart)
    .bind(c.shop.origin.as_str())
    .bind(ana_id)
    .bind(&view["cart"]["version"])
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    drop(tx);
    assert_eq!(ttl, 300.0, "5 minutes");
    assert!(
        bound,
        "bound to the capability, origin, generation, expected account and reviewed version"
    );
    let code = c.placement_code(None, &state).await;
    c.exec("UPDATE checkout_placements SET expires_at = now() - interval '1 second'")
        .await;
    c.assert_aborted(
        &c.edge,
        &cart,
        json!({ "state": state, "code": code }),
        "an expired state",
    )
    .await;

    let state = c.placement_state(&cart, "ttl-2", &view, "").await;
    let code = c.placement_code(None, &state).await;
    c.exec("UPDATE account_codes SET expires_at = now() - interval '1 second'")
        .await;
    c.assert_aborted(
        &c.edge,
        &cart,
        json!({ "state": state, "code": code }),
        "an expired code",
    )
    .await;
    assert_eq!(c.orders().await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn changed_mode_or_generation_aborts(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let (cart, view) = c.linked(method, &ana).await;
    let state = c.placement_state(&cart, "mode", &view, "").await;
    let code = c.placement_code(Some(&ana), &state).await;

    c.switch(CheckoutMode::Isolated).await;
    c.assert_aborted(
        &c.edge,
        &cart,
        json!({ "state": state, "code": code }),
        "the origin is isolated now",
    )
    .await;
    c.switch(CheckoutMode::SameOrigin).await;
    c.assert_aborted(
        &c.edge,
        &cart,
        json!({ "state": state, "code": code }),
        "two switches moved the generation",
    )
    .await;
    assert_eq!(c.unconsumed_placement().await, (1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn rotation_after_place_post_aborts(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let (cart, view) = c.linked(method, &ana).await;
    let state = c.placement_state(&cart, "rotate", &view, "").await;
    let code = c.placement_code(Some(&ana), &state).await;

    let mut other_tab = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let ctx = c.context(&mut other_tab).await;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *other_tab)
        .await
        .unwrap();
    let rotated = checkout_access::rotate(&mut other_tab, &ctx, &cart)
        .await
        .unwrap();

    let callback = tokio::spawn({
        let (runtime, tenant, cz, origin, settings) = (
            c.runtime.clone(),
            c.shop.tenant,
            c.shop.cz,
            c.shop.origin.clone(),
            c.s.checkout.clone(),
        );
        let (cart, state, code) = (cart.clone(), state.clone(), code.clone());
        async move {
            let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
            let mut ctx =
                storefront::context(&mut tx, &PublicUrls::default(), cz, None, Utc::now())
                    .await
                    .unwrap();
            ctx.origin = Some(origin);
            checkout_identity::confirm_placement(
                &mut tx, &ctx, &settings, &cart, &state, &code, None,
            )
            .await
            .map(|_| ())
        }
    });
    waiting_on(&c.db, pid).await;
    other_tab.commit().await.unwrap();
    assert!(
        matches!(callback.await.unwrap(), Err(platform::Error::NotFound)),
        "the callback queued behind the other tab's rotation finds nothing"
    );
    c.assert_aborted(
        &c.edge,
        &rotated,
        json!({ "state": state, "code": code }),
        "the rotated capability",
    )
    .await;
    assert_eq!(c.orders().await, 0);
    assert_eq!(c.unconsumed_placement().await, (1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn duplicate_callbacks_create_one_order(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let (cart, view) = c.linked(method, &ana).await;
    let state = c.placement_state(&cart, "twice", &view, "").await;
    let code = c.placement_code(Some(&ana), &state).await;

    let mut first = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let ctx = c.context(&mut first).await;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *first)
        .await
        .unwrap();
    let confirmed = checkout_identity::confirm_placement(
        &mut first,
        &ctx,
        &c.s.checkout,
        &cart,
        &state,
        &code,
        None,
    )
    .await
    .unwrap();
    assert!(!confirmed.as_guest);

    let second = tokio::spawn({
        let (runtime, tenant, cz, origin, settings) = (
            c.runtime.clone(),
            c.shop.tenant,
            c.shop.cz,
            c.shop.origin.clone(),
            c.s.checkout.clone(),
        );
        let (cart, state, code) = (cart.clone(), state.clone(), code.clone());
        async move {
            let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
            let mut ctx =
                storefront::context(&mut tx, &PublicUrls::default(), cz, None, Utc::now())
                    .await
                    .unwrap();
            ctx.origin = Some(origin);
            checkout_identity::confirm_placement(
                &mut tx, &ctx, &settings, &cart, &state, &code, None,
            )
            .await
            .map(|_| ())
        }
    });
    waiting_on(&c.db, pid).await;
    first.commit().await.unwrap();
    assert!(
        matches!(second.await.unwrap(), Err(platform::Error::NotFound)),
        "the duplicate queued behind the winner finds nothing"
    );
    c.assert_aborted(
        &c.edge,
        &cart,
        json!({ "state": state, "code": code }),
        "a later duplicate",
    )
    .await;
    assert_eq!(c.orders().await, 1);
}

const FAIL_ORDER_INSERT: &str = "
CREATE FUNCTION placement_fail() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'injected failure on the order insert';
END
$$;
CREATE TRIGGER placement_fail BEFORE INSERT ON orders
    FOR EACH ROW EXECUTE FUNCTION placement_fail();
";

#[sqlx::test(migrations = "../../migrations")]
async fn failed_order_insert_rolls_back_state_code_and_stock(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let (cart, view) = c.linked(method, &ana).await;
    let state = c.placement_state(&cart, "fail", &view, "").await;
    let code = c.placement_code(Some(&ana), &state).await;
    let stock = "SELECT coalesce(sum(reserved), 0)::int8 FROM inventory_levels";
    let movements = "SELECT count(*) FROM stock_movements";
    let before = (c.count(stock).await, c.count(movements).await);

    sqlx::raw_sql(FAIL_ORDER_INSERT)
        .execute(&c.db)
        .await
        .unwrap();
    let (status, body) = c.confirm(&cart, &state, &code).await;
    assert!(status.is_server_error(), "{status} {body}");
    assert_eq!(
        c.unconsumed_placement().await,
        (1, 1),
        "state and code stay"
    );
    assert_eq!(
        (c.count(stock).await, c.count(movements).await),
        before,
        "no stock reserved"
    );
    assert_eq!(c.orders().await, 0);

    sqlx::raw_sql("DROP TRIGGER placement_fail ON orders")
        .execute(&c.db)
        .await
        .unwrap();
    let (status, body) = c.confirm(&cart, &state, &code).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(c.unconsumed_placement().await, (0, 0));
    assert!(c.count(stock).await > before.0, "the retry reserved stock");
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_note_is_the_one_reviewed_when_the_state_was_created(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let (cart, view) = c.linked(method, &ana).await;
    let first = c
        .placement_state(&cart, "note-1", &view, "Leave it at the door")
        .await;
    let first_code = c.placement_code(Some(&ana), &first).await;
    let second = c
        .placement_state(&cart, "note-2", &view, "Ring twice")
        .await;
    let second_code = c.placement_code(Some(&ana), &second).await;
    c.assert_aborted(
        &c.edge,
        &cart,
        json!({ "state": first, "code": first_code }),
        "a later place-order revoked the earlier state",
    )
    .await;

    let (status, text, k) = c
        .confirm_as(
            &c.edge,
            &cart,
            json!({ "state": second, "code": second_code }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    let order: Uuid = body["order_id"].as_str().unwrap().parse().unwrap();
    let notes: Option<String> = c
        .read("SELECT notes FROM orders WHERE id = $1", order)
        .await;
    assert_eq!(notes.as_deref(), Some("Ring twice"));
    let k = k.expect("the note replaced the capability (C6)");
    assert!(commerce::capability::well_formed(&k) && k != cart);
}

#[sqlx::test(migrations = "../../migrations")]
async fn changed_mode_without_a_generation_bump_aborts(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let (cart, view) = c.linked(method, &ana).await;
    let state = c.placement_state(&cart, "mode-only", &view, "").await;
    let code = c.placement_code(Some(&ana), &state).await;
    let generation = async || -> i64 {
        sqlx::query_scalar("SELECT generation FROM platform.checkout_fences WHERE tenant_id = $1")
            .bind(c.shop.tenant)
            .fetch_one(&c.db)
            .await
            .unwrap()
    };
    let before = generation().await;

    sqlx::query(
        "ALTER TABLE platform.checkout_origins DISABLE TRIGGER checkout_policy_mode_updated",
    )
    .execute(&c.db)
    .await
    .unwrap();
    sqlx::query("UPDATE platform.checkout_origins SET mode = 'isolated' WHERE origin = $1")
        .bind(c.shop.origin.as_str())
        .execute(&c.db)
        .await
        .unwrap();
    sqlx::query(
        "ALTER TABLE platform.checkout_origins ENABLE TRIGGER checkout_policy_mode_updated",
    )
    .execute(&c.db)
    .await
    .unwrap();
    assert_eq!(
        generation().await,
        before,
        "the mode moved, the generation did not"
    );

    c.assert_aborted(
        &c.edge,
        &cart,
        json!({ "state": state, "code": code }),
        "the mode changed under the state",
    )
    .await;
    assert_eq!(c.unconsumed_placement().await, (1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn changed_version_aborts(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let (cart, view) = c.linked(method, &ana).await;
    let state = c.placement_state(&cart, "version", &view, "").await;
    let code = c.placement_code(Some(&ana), &state).await;

    let changed = c.ready(&cart, method).await;
    assert_ne!(
        changed["cart"]["version"], view["cart"]["version"],
        "a non-PII write moved the version without a rotation"
    );
    let (status, text, _) = c
        .confirm_as(&c.edge, &cart, json!({ "state": state, "code": code }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(super::code(&text), "cart_changed");
    assert_eq!(c.orders().await, 0);
    assert_eq!(c.unconsumed_placement().await, (1, 1));
}
