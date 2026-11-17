use super::pii_rotation::{addresses, contact, holds_a_capability, place_body};
use super::*;

const ATTACKER_STATE: &str = "attackerstate_0123456789abcdef0123456789abcdef0123456789abcdef";

pub(super) struct Started {
    pub(super) state: String,
}

impl Ctx {
    pub(super) async fn signed_in(&self, email: &str) -> (String, Uuid) {
        let (status, _, _) = self
            .edge
            .call(Call::post(
                "/storefront/v1/customer/magic-link",
                json!({ "email": email }),
            ))
            .send(&self.s)
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        let text: String = sqlx::query_scalar(
            "SELECT body_text FROM email_messages WHERE template = 'magic_link' AND to_email = $1
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(email)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let at = text.find("token=").unwrap() + "token=".len();
        let (status, body, res) = self
            .edge
            .call(Call::post(
                "/storefront/v1/customer/magic-link/consume",
                json!({ "token": &text[at..at + 64] }),
            ))
            .send(&self.s)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let session = res.headers()["x-session-token"]
            .to_str()
            .unwrap()
            .to_owned();
        let id = body["customer"]["id"].as_str().unwrap().parse().unwrap();
        (session, id)
    }

    async fn save_address(&self, session: &str, name: &str) {
        let (status, body, _) = self
            .edge
            .call(Call::post(
                "/storefront/v1/customer/addresses",
                json!({ "name": name, "street": "Saved 1", "city": "Brno",
                        "postal_code": "602 00", "country": "CZ", "is_default": true }),
            ))
            .header("x-customer-session", session.to_owned())
            .send(&self.s)
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    async fn identify_start(&self, cart: &str) -> (StatusCode, Value) {
        self.call(
            Call::post("/storefront/v1/checkout/identify", json!({})),
            cart,
        )
        .await
    }

    pub(super) async fn identify(&self, cart: &str) -> Started {
        let (status, body) = self.identify_start(cart).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        Started {
            state: body["state"].as_str().unwrap().to_owned(),
        }
    }

    pub(super) async fn code(&self, session: &str, purpose: &str, state: &str) -> String {
        let (status, body, _) = self
            .edge
            .call(Call::post(
                "/storefront/v1/customer/account-codes",
                json!({ "purpose": purpose, "state": state, "origin": self.edge.origin }),
            ))
            .header("x-customer-session", session.to_owned())
            .send(&self.s)
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["code"].as_str().unwrap().to_owned()
    }

    async fn callback(&self, cart: &str, body: Value) -> (StatusCode, String, Option<String>) {
        let (status, text, res) = self
            .edge
            .call(Call::post(
                "/storefront/v1/checkout/identify/callback",
                body,
            ))
            .header("x-cart-token", cart.to_owned())
            .send_text(&self.s)
            .await;
        (status, text, replacement(&res))
    }

    pub(super) async fn identified(&self, cart: &str, state: &str, code: &str) -> String {
        let (status, text, k) = self
            .callback(cart, json!({ "state": state, "idt": state, "code": code }))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
        assert_eq!(text, "");
        let k = k.expect("identify replaced the capability");
        assert!(commerce::capability::well_formed(&k) && k != cart);
        k
    }

    async fn saved(&self, cart: &str) -> (StatusCode, Value) {
        self.call(Call::get("/storefront/v1/checkout/saved-addresses"), cart)
            .await
    }

    async fn saved_names(&self, cart: &str) -> Vec<String> {
        let (status, body) = self.saved(cart).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_owned())
            .collect()
    }

    pub(super) async fn count(&self, sql: &'static str) -> i64 {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        sqlx::query_scalar(sql).fetch_one(&mut *tx).await.unwrap()
    }

    pub(super) async fn exec(&self, sql: &'static str) {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }

    async fn unconsumed(&self) -> (i64, i64) {
        (
            self.count("SELECT count(*) FROM checkout_identities WHERE consumed_at IS NULL")
                .await,
            self.count("SELECT count(*) FROM account_codes WHERE consumed_at IS NULL")
                .await,
        )
    }

    async fn assert_denied(&self, cart: &str, body: Value, what: &str) {
        let (status, text, k) = self.callback(cart, body).await;
        assert!(status.is_client_error(), "{what}: {status} {text}");
        assert_eq!(k, None, "{what}: no capability");
        assert!(!holds_a_capability(&text), "{what}: {text}");
    }
}

pub(super) async fn checkout_cart(c: &Ctx) -> String {
    let shop = c.shop_cart().await;
    c.started(&shop, None).await
}

#[sqlx::test(migrations = "../../migrations")]
async fn identify_start_mints_a_fresh_five_minute_state_bound_to_the_capability(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let cart = checkout_cart(&c).await;
    let first = c.identify(&cart).await;
    assert!(commerce::capability::well_formed(&first.state));
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let (ttl, bound): (f64, bool) = sqlx::query_as(
        "SELECT extract(epoch FROM expires_at - created_at)::float8,
                capability_hash = sha256($1::text::bytea) AND origin = $2 AND generation > 0
         FROM checkout_identities",
    )
    .bind(&cart)
    .bind(c.shop.origin.as_str())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    drop(tx);
    assert_eq!(ttl, 300.0, "5 minutes");
    assert!(
        bound,
        "bound to the capability, exact origin and generation"
    );

    let second = c.identify(&cart).await;
    assert_ne!(first.state, second.state, "every start mints a fresh state");
    assert_eq!(
        c.unconsumed().await.0,
        1,
        "a new start revokes the checkout's earlier state"
    );
    let (status, _) = c.checkout(&cart).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "starting identify keeps the capability"
    );

    let (status, _) = c.identify_start(&"f".repeat(64)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown capability");
    c.switch(CheckoutMode::Isolated).await;
    let (status, body) = c.identify_start(&cart).await;
    assert!(
        status == StatusCode::CONFLICT || status == StatusCode::NOT_FOUND,
        "isolated mode has no identify: {status} {body}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn unsolicited_identify_rejected(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let cart = checkout_cart(&c).await;
    let (attacker, _) = c.signed_in("attacker@example.cz").await;
    let code = c.code(&attacker, "identify", ATTACKER_STATE).await;

    c.assert_denied(
        &cart,
        json!({ "state": ATTACKER_STATE, "idt": ATTACKER_STATE, "code": code }),
        "a callback for a state this checkout never started",
    )
    .await;
    let started = c.identify(&cart).await;
    c.assert_denied(
        &cart,
        json!({ "state": started.state, "idt": started.state, "code": code }),
        "a code issued for another state",
    )
    .await;
    let (status, _) = c.checkout(&cart).await;
    assert_eq!(status, StatusCode::OK, "the capability survives");
    assert_eq!(c.saved_names(&cart).await, Vec::<String>::new());
    assert_eq!(c.unconsumed().await, (1, 1), "nothing consumed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn missing_wrong_cookie_code_state_denied(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let cart = checkout_cart(&c).await;
    let other = checkout_cart(&c).await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let s = c.identify(&cart).await.state;
    let other_state = c.identify(&other).await.state;
    let code = c.code(&ana, "identify", &s).await;
    let placement = c.code(&ana, "placement", &s).await;
    let other_code = c.code(&ana, "identify", &other_state).await;

    for (body, what) in [
        (json!({ "state": s, "code": code }), "no idt cookie"),
        (
            json!({ "state": s, "idt": "", "code": code }),
            "empty idt cookie",
        ),
        (
            json!({ "state": s, "idt": other_state, "code": code }),
            "another checkout's idt",
        ),
        (
            json!({ "state": s, "idt": ATTACKER_STATE, "code": code }),
            "a planted idt",
        ),
        (json!({ "idt": s, "code": code }), "no state"),
        (
            json!({ "state": other_state, "idt": s, "code": other_code }),
            "state and idt differ",
        ),
        (json!({ "state": s, "idt": s }), "no code"),
        (
            json!({ "state": s, "idt": s, "code": "f".repeat(64) }),
            "an unknown code",
        ),
        (
            json!({ "state": s, "idt": s, "code": placement }),
            "a placement code",
        ),
        (
            json!({ "state": s, "idt": s, "code": other_code }),
            "a code for another state",
        ),
    ] {
        c.assert_denied(&cart, body, what).await;
    }
    c.assert_denied(
        &other,
        json!({ "state": s, "idt": s, "code": code }),
        "the state of another checkout's capability",
    )
    .await;
    c.assert_denied(
        &"f".repeat(64),
        json!({ "state": s, "idt": s, "code": code }),
        "no capability",
    )
    .await;
    assert_eq!(c.unconsumed().await, (2, 3), "no refusal consumed anything");
    c.identified(&cart, &s, &code).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn code_replay_and_expiry_denied(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let cart = checkout_cart(&c).await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let s = c.identify(&cart).await.state;
    let code = c.code(&ana, "identify", &s).await;
    let k = c.identified(&cart, &s, &code).await;
    c.assert_denied(
        &k,
        json!({ "state": s, "idt": s, "code": code }),
        "a replay with the new capability",
    )
    .await;
    c.assert_denied(
        &cart,
        json!({ "state": s, "idt": s, "code": code }),
        "a replay with the old capability",
    )
    .await;

    let s = c.identify(&k).await.state;
    let code = c.code(&ana, "identify", &s).await;
    c.exec("UPDATE checkout_identities SET expires_at = now() - interval '1 second'")
        .await;
    c.assert_denied(
        &k,
        json!({ "state": s, "idt": s, "code": code }),
        "an expired state",
    )
    .await;

    let s = c.identify(&k).await.state;
    let code = c.code(&ana, "identify", &s).await;
    c.exec("UPDATE account_codes SET expires_at = now() - interval '1 second'")
        .await;
    c.assert_denied(
        &k,
        json!({ "state": s, "idt": s, "code": code }),
        "an expired code",
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_callbacks_have_one_winner(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let cart = checkout_cart(&c).await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let s = c.identify(&cart).await.state;
    let code = c.code(&ana, "identify", &s).await;

    let mut first = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let mut ctx = storefront::context(
        &mut first,
        &PublicUrls::default(),
        c.shop.cz,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    ctx.origin = Some(c.shop.origin.clone());
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *first)
        .await
        .unwrap();
    commerce::checkout_identity::callback(&mut first, &ctx, &cart, &s, &s, &code)
        .await
        .unwrap();

    let second = tokio::spawn({
        let (runtime, tenant, cz, origin) = (
            c.runtime.clone(),
            c.shop.tenant,
            c.shop.cz,
            c.shop.origin.clone(),
        );
        let (cart, s, code) = (cart.clone(), s.clone(), code.clone());
        async move {
            let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
            let mut ctx =
                storefront::context(&mut tx, &PublicUrls::default(), cz, None, Utc::now())
                    .await
                    .unwrap();
            ctx.origin = Some(origin);
            commerce::checkout_identity::callback(&mut tx, &ctx, &cart, &s, &s, &code).await
        }
    });
    waiting_on(&c.db, pid).await;
    first.commit().await.unwrap();
    assert!(
        matches!(second.await.unwrap(), Err(platform::Error::NotFound)),
        "the callback queued behind the winner finds nothing"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn rotated_generation_invalidates_identify(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let cart = checkout_cart(&c).await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    let s = c.identify(&cart).await.state;
    let code = c.code(&ana, "identify", &s).await;
    let k = c.rotated(&cart, contact("victim@example.cz")).await;
    c.assert_denied(
        &k,
        json!({ "state": s, "idt": s, "code": code }),
        "a PII write rotated the capability the state was bound to",
    )
    .await;
    c.assert_denied(
        &cart,
        json!({ "state": s, "idt": s, "code": code }),
        "the revoked capability",
    )
    .await;

    let s = c.identify(&k).await.state;
    let code = c.code(&ana, "identify", &s).await;
    c.switch(CheckoutMode::Isolated).await;
    c.switch(CheckoutMode::SameOrigin).await;
    c.assert_denied(
        &k,
        json!({ "state": s, "idt": s, "code": code }),
        "a mode switch changed the generation",
    )
    .await;
    assert_eq!(c.unconsumed().await.1, 2, "no code was consumed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn identify_prefill_has_no_account_visibility_or_side_effects(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let cart = checkout_cart(&c).await;
    let (ana, ana_id) = c.signed_in("ana@example.cz").await;
    c.save_address(&ana, "Ana Saved").await;
    let before = c.account_state(&ana, ana_id).await;
    assert_eq!(
        c.saved_names(&cart).await,
        Vec::<String>::new(),
        "no prefill before identify"
    );

    let s = c.identify(&cart).await.state;
    let code = c.code(&ana, "identify", &s).await;
    let k = c.identified(&cart, &s, &code).await;
    let (status, _) = c.checkout(&cart).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "identify rotated the capability (C6)"
    );
    let (status, _) = c.saved(&cart).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the old capability reads no prefill"
    );
    let (status, body) = c.saved(&k).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"][0]["name"], "Ana Saved");
    assert_eq!(body["items"][0]["street"], "Saved 1");

    let k = c.rotated(&k, contact("guest@example.cz")).await;
    let k = c.rotated(&k, addresses("Typed Name")).await;
    assert_eq!(
        c.saved_names(&k).await,
        ["Ana Saved"],
        "the hint survives rotation"
    );
    let view = c.ready(&k, method).await;
    let (status, text, _) = c.place(&k, "identify-order", &place_body(&view, "")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    let state = serde_json::from_str::<Value>(&text).unwrap()["state"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body, _) = c
        .edge
        .call(Call::post(
            "/storefront/v1/customer/account-codes",
            json!({ "purpose": "placement", "state": state, "origin": c.edge.origin }),
        ))
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, text, _) = c
        .edge
        .call(Call::post(
            "/storefront/v1/checkout/placement/callback",
            json!({ "state": state, "code": body["code"] }),
        ))
        .header("x-cart-token", k.clone())
        .send_text(&c.s)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    assert_eq!(
        c.account_state(&ana, ana_id).await,
        before,
        "the account saw nothing"
    );
    assert_eq!(
        c.count("SELECT count(*) FROM carts WHERE customer_id IS NOT NULL")
            .await,
        0,
        "no cart is attached to the account"
    );
    assert_eq!(
        c.count("SELECT count(*) FROM orders WHERE customer_id IS NOT NULL")
            .await,
        0,
        "the order is a guest order"
    );
}

#[derive(Debug, PartialEq)]
struct AccountState {
    me: Value,
    orders: Value,
    addresses: Value,
    sessions: i64,
    consent: i64,
    customer_updated: chrono::DateTime<Utc>,
}

impl Ctx {
    async fn account_state(&self, session: &str, id: Uuid) -> AccountState {
        let get = async |path: &'static str| {
            let (status, body, _) = self
                .edge
                .call(Call::get(path))
                .header("x-customer-session", session.to_owned())
                .send(&self.s)
                .await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            body
        };
        AccountState {
            me: get("/storefront/v1/customer/me").await,
            orders: get("/storefront/v1/customer/orders").await,
            addresses: get("/storefront/v1/customer/addresses").await,
            sessions: self.count("SELECT count(*) FROM customer_sessions").await,
            consent: self.count("SELECT count(*) FROM consent_records").await,
            customer_updated: self
                .read("SELECT updated_at FROM customers WHERE id = $1", id)
                .await,
        }
    }
}

const FAIL_IDENTIFY_ROTATION: &str = "
CREATE FUNCTION identify_fail() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM checkout_identities
               WHERE cart_id = OLD.id AND consumed_at IS NOT NULL) THEN
        RAISE EXCEPTION 'injected failure after the identify consumption';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER identify_fail BEFORE UPDATE OF checkout_token_hash ON carts
    FOR EACH ROW EXECUTE FUNCTION identify_fail();
";

#[sqlx::test(migrations = "../../migrations")]
async fn failed_identify_rolls_back_state_and_code_consumption(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let cart = checkout_cart(&c).await;
    let (ana, _) = c.signed_in("ana@example.cz").await;
    c.save_address(&ana, "Ana Saved").await;
    let s = c.identify(&cart).await.state;
    let code = c.code(&ana, "identify", &s).await;

    sqlx::raw_sql(FAIL_IDENTIFY_ROTATION)
        .execute(&c.db)
        .await
        .unwrap();
    let (status, text, k) = c
        .callback(&cart, json!({ "state": s, "idt": s, "code": code }))
        .await;
    assert!(status.is_server_error(), "{status} {text}");
    assert_eq!(k, None);
    assert_eq!(
        c.unconsumed().await,
        (1, 1),
        "state and code stay unconsumed"
    );
    assert_eq!(
        c.saved_names(&cart).await,
        Vec::<String>::new(),
        "no prefill link"
    );
    let (status, _) = c.checkout(&cart).await;
    assert_eq!(status, StatusCode::OK, "the capability did not rotate");

    sqlx::raw_sql("DROP TRIGGER identify_fail ON carts")
        .execute(&c.db)
        .await
        .unwrap();
    let k = c.identified(&cart, &s, &code).await;
    assert_eq!(c.saved_names(&k).await, ["Ana Saved"]);
}
