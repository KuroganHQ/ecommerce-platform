use std::sync::Arc;

use tokio::sync::Mutex;

use super::*;

pub(super) const OWNER: &str = "owner";

fn jwt(sub: &str, auth_age: i64) -> String {
    let mut claims = claims(sub);
    claims["auth_time"] = json!(now() - auth_age);
    sign(&claims)
}

impl Ctx {
    pub(super) async fn admin(&self, call: Call<'_>, sub: &str) -> (StatusCode, Value) {
        self.admin_on(&self.s, call, sub).await
    }

    async fn admin_on(&self, s: &api::AppState, call: Call<'_>, sub: &str) -> (StatusCode, Value) {
        let token = jwt(sub, 60);
        let (status, body, _) = call.token(&token).tenant(self.shop.tenant).send(s).await;
        (status, body)
    }

    pub(super) async fn attest(&self, origin: &str) -> Value {
        let (status, body) = self
            .admin(
                Call::post(
                    "/admin/v1/checkout-origins/history",
                    json!({ "origin": origin, "attestation_version": 1 }),
                ),
                OWNER,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    pub(super) async fn set_mode(&self, origin: &str, mode: &str) -> (StatusCode, Value) {
        self.admin(
            Call::post(
                "/admin/v1/checkout-origins/mode",
                json!({ "origin": origin, "mode": mode }),
            ),
            OWNER,
        )
        .await
    }

    async fn origins(&self) -> Vec<Value> {
        let (status, body) = self
            .admin(Call::get("/admin/v1/checkout-origins"), OWNER)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["items"].as_array().unwrap().clone()
    }

    pub(super) async fn origin(&self, origin: &str) -> Value {
        self.origins()
            .await
            .into_iter()
            .find(|o| o["origin"] == origin)
            .unwrap_or_else(|| panic!("{origin} is listed"))
    }

    async fn platform_host(&self, host: &str, complete: bool) -> String {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO platform.hostname_claims (hostname, tenant_id, platform_issued, claimed_at)
             VALUES ($1, $2, true, CASE WHEN $3 THEN now() END)",
        )
        .bind(host)
        .bind(self.shop.tenant)
        .bind(complete)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO platform.domains (hostname, tenant_id, market_id, is_primary, verified_at)
             VALUES ($1, $2, $3, false, now())",
        )
        .bind(host)
        .bind(self.shop.tenant)
        .bind(self.shop.cz)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        format!("https://{host}")
    }

    pub(super) async fn alias(&self, host: &str) -> Edge {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO platform.domains (hostname, tenant_id, market_id, is_primary, verified_at)
             VALUES ($1, $2, $3, false, now())",
        )
        .bind(host)
        .bind(self.shop.tenant)
        .bind(self.shop.cz)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        Edge {
            origin: format!("https://{host}"),
            ..self.edge.clone()
        }
    }

    pub(super) async fn handed_off_via(&self, edge: &Edge, variant: Uuid) -> String {
        let (status, _, res) = edge
            .call(Call::post("/storefront/v1/cart", json!({})))
            .send(&self.s)
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let shop_token = res.headers()["x-cart-token"].to_str().unwrap().to_owned();
        for call in [
            Call::post(
                "/storefront/v1/cart/lines",
                json!({ "variant_id": variant }),
            ),
            Call::post("/storefront/v1/cart/handoff", json!({})),
        ] {
            let (status, body, _) = edge
                .call(call)
                .header("x-cart-token", shop_token.clone())
                .send(&self.s)
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            if let Some(handoff) = body.get("token") {
                let (status, body, _) = edge
                    .call(Call::post(
                        "/storefront/v1/checkout/handoff",
                        json!({ "token": handoff }),
                    ))
                    .send(&self.s)
                    .await;
                assert_eq!(status, StatusCode::OK, "{body}");
                return body["cart_token"].as_str().unwrap().to_owned();
            }
        }
        unreachable!("the handoff answers a token")
    }

    pub(super) async fn checkout_via(&self, edge: &Edge, cart: &str) -> StatusCode {
        edge.call(Call::get("/storefront/v1/checkout"))
            .header("x-cart-token", cart.to_owned())
            .send(&self.s)
            .await
            .0
    }

    async fn audit(&self, action: &str) -> Vec<Value> {
        let (status, body) = self
            .admin(Call::get("/admin/v1/audit-log?limit=100"), OWNER)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["action"] == action)
            .cloned()
            .collect()
    }
}

async fn purge_sink() -> (api::edge::EdgePurge, Arc<Mutex<Vec<Value>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = axum::Router::new().route(
        "/_edge/purge",
        axum::routing::post({
            let seen = seen.clone();
            async move |axum::Json(body): axum::Json<Value>| {
                seen.lock().await.push(body);
                "{}"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/_edge/purge", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let edge = api::edge::EdgePurge::new(
        Some(url.parse().unwrap()),
        "edge-purge-token-0123456789".into(),
    );
    (edge, seen)
}

#[sqlx::test(migrations = "../../migrations")]
async fn attestation_records_actor_time_and_version(db: PgPool) {
    let c = setup(db, CheckoutMode::Isolated).await;
    let origin = c.shop.origin.as_str();
    let before = c.origin(origin).await;
    assert_eq!(
        before,
        json!({ "origin": origin, "mode": "isolated", "mode_generation": 1,
                "history": "unknown_or_used", "attestation": null })
    );

    let started = Utc::now();
    let attested = c.attest(origin).await;
    assert_eq!(attested["history"], "merchant_attested_never_served");
    assert_eq!(attested["attestation"]["by"], OWNER);
    assert_eq!(attested["attestation"]["version"], 1);
    let at: chrono::DateTime<Utc> = attested["attestation"]["at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(at >= started - chrono::Duration::seconds(5) && at <= Utc::now());
    assert_eq!(
        (
            attested["mode"].clone(),
            attested["mode_generation"].clone()
        ),
        (json!("isolated"), json!(1)),
        "recording the history changes no mode"
    );
    assert_eq!(c.origin(origin).await, attested, "the list shows it");
    let audit = c.audit("checkout_origin.history_recorded").await;
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert_eq!(audit[0]["actor"], OWNER);
    assert_eq!(audit[0]["entity_id"], origin);
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_or_used_origins_cannot_enable(db: PgPool) {
    let c = setup(db, CheckoutMode::Isolated).await;
    let origin = c.shop.origin.as_str();
    let (status, body) = c.set_mode(origin, "same_origin").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("origin_history_unknown")),
        "{body}"
    );

    let used = c.platform_host("used-platform.localhost", false).await;
    let (status, body) = c
        .admin(
            Call::post(
                "/admin/v1/checkout-origins/history",
                json!({ "origin": used }),
            ),
            OWNER,
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("origin_history_unknown")),
        "a platform hostname without its full claim: {body}"
    );
    let (status, body) = c.set_mode(&used, "same_origin").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let fresh = c.platform_host("fresh-platform.localhost", true).await;
    let (status, body) = c
        .admin(
            Call::post(
                "/admin/v1/checkout-origins/history",
                json!({ "origin": fresh, "attestation_version": 1 }),
            ),
            OWNER,
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("platform_hostname")),
        "a merchant cannot attest a platform hostname: {body}"
    );
    let (status, body) = c
        .admin(
            Call::post(
                "/admin/v1/checkout-origins/history",
                json!({ "origin": fresh }),
            ),
            OWNER,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["history"], "platform_never_reassigned");
    assert_eq!(body["attestation"], Value::Null);

    for (input, code) in [
        (json!({ "origin": origin }), "attestation_required"),
        (
            json!({ "origin": origin, "attestation_version": 2 }),
            "unsupported_attestation_version",
        ),
        (
            json!({ "origin": "https://Shop.localhost" }),
            "invalid_origin",
        ),
    ] {
        let (status, body) = c
            .admin(
                Call::post("/admin/v1/checkout-origins/history", input.clone()),
                OWNER,
            )
            .await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(code)),
            "{input}: {body}"
        );
    }
    let other = testkit::storefront::shop(&c.runtime, "other").await;
    for call in [
        Call::post(
            "/admin/v1/checkout-origins/history",
            json!({ "origin": other.origin.as_str(), "attestation_version": 1 }),
        ),
        Call::post(
            "/admin/v1/checkout-origins/mode",
            json!({ "origin": other.origin.as_str(), "mode": "isolated" }),
        ),
    ] {
        let (status, body) = c.admin(call, OWNER).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "another tenant's origin: {body}"
        );
    }
    assert_eq!(c.origin(origin).await["mode"], "isolated");
}

#[sqlx::test(migrations = "../../migrations")]
async fn admin_api_rejects_forged_readiness_history_and_unauthorized_actors(db: PgPool) {
    let c = setup(db, CheckoutMode::Isolated).await;
    let origin = c.shop.origin.as_str();
    for forged in [
        json!({ "origin": origin, "history": "platform_never_reassigned" }),
        json!({ "origin": origin, "attestation_version": 1, "attested_by": "someone-else" }),
        json!({ "origin": origin, "attestation_version": 1, "attested_at": "2020-01-01T00:00:00Z" }),
    ] {
        let (status, body) = c
            .admin(
                Call::post("/admin/v1/checkout-origins/history", forged.clone()),
                OWNER,
            )
            .await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_body")),
            "{forged}: {body}"
        );
    }
    assert_eq!(c.origin(origin).await["history"], "unknown_or_used");

    c.attest(origin).await;
    let mut unready = c.s.clone();
    unready.same_origin_ready = false;
    let (status, body) = c
        .admin_on(
            &unready,
            Call::post(
                "/admin/v1/checkout-origins/mode",
                json!({ "origin": origin, "mode": "same_origin", "ready": true }),
            ),
            OWNER,
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_body")),
        "a forged readiness claim: {body}"
    );
    let (status, body) = c
        .admin_on(
            &unready,
            Call::post(
                "/admin/v1/checkout-origins/mode",
                json!({ "origin": origin, "mode": "same_origin" }),
            ),
            OWNER,
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("checkout_not_ready")),
        "{body}"
    );
    assert_eq!(c.origin(origin).await["mode"], "isolated");

    testkit::staff(&c.runtime, c.shop.tenant, "clerk", "staff").await;
    testkit::staff(&c.runtime, c.shop.tenant, "manager", "admin").await;
    let other = testkit::storefront::shop(&c.runtime, "other").await;
    testkit::staff(&c.runtime, other.tenant, "owner-b", "owner").await;
    let stale = jwt(OWNER, 3600);
    for path in [
        "/admin/v1/checkout-origins/history",
        "/admin/v1/checkout-origins/mode",
    ] {
        let body = json!({ "origin": origin, "attestation_version": 1, "mode": "same_origin" });
        for (who, expected, code) in [
            ("clerk", StatusCode::FORBIDDEN, "insufficient_role"),
            ("manager", StatusCode::FORBIDDEN, "insufficient_role"),
            ("owner-b", StatusCode::FORBIDDEN, "not_a_member"),
        ] {
            let mut input = body.clone();
            if path.ends_with("history") {
                input.as_object_mut().unwrap().remove("mode");
            } else {
                input.as_object_mut().unwrap().remove("attestation_version");
            }
            let (status, problem) = c.admin(Call::post(path, input), who).await;
            assert_eq!(
                (status, problem["code"].as_str()),
                (expected, Some(code)),
                "{who} on {path}: {problem}"
            );
        }
        let (status, problem, _) = Call::post(path, json!({ "origin": origin }))
            .token(&stale)
            .tenant(c.shop.tenant)
            .send(&c.s)
            .await;
        assert_eq!(
            (status, problem["code"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("reauth_required")),
            "a stale sign-in on {path}"
        );
    }
    let (status, _) = c
        .admin(Call::get("/admin/v1/checkout-origins"), "clerk")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff cannot list policies");
    let listed = c.origin(origin).await;
    assert_eq!(
        (listed["mode"].clone(), listed["mode_generation"].clone()),
        (json!("isolated"), json!(1)),
        "nothing switched"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn switch_revokes_the_shop_audits_and_purges(db: PgPool) {
    let mut c = setup(db, CheckoutMode::Isolated).await;
    let (edge, purged) = purge_sink().await;
    c.s.edge = edge;
    let origin = c.shop.origin.as_str().to_owned();
    let alias = c.alias("alias.localhost").await;
    let here = c.handed_off_via(&c.edge, c.shop.variants[0]).await;
    let there = c.handed_off_via(&alias, c.shop.variants[0]).await;
    let other = testkit::storefront::shop(&c.runtime, "other").await;
    let other_edge = Edge {
        token: other.token.clone(),
        market: other.cz,
        origin: other.origin.as_str().to_owned(),
    };
    let elsewhere = c.handed_off_via(&other_edge, other.variants[0]).await;
    c.attest(&origin).await;

    let (status, body) = c.set_mode(&origin, "same_origin").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["mode"].clone(), body["mode_generation"].clone()),
        (json!("same_origin"), json!(2))
    );
    assert_eq!(
        purged.lock().await.clone(),
        vec![json!({ "tenant_id": c.shop.tenant })],
        "the edge drops its resolver and page caches of the tenant"
    );
    assert_eq!(
        c.checkout_via(&c.edge, &here).await,
        StatusCode::NOT_FOUND,
        "revoked"
    );
    assert_eq!(
        c.checkout_via(&alias, &there).await,
        StatusCode::NOT_FOUND,
        "the alias is revoked too"
    );
    assert_eq!(
        c.checkout_via(&other_edge, &elsewhere).await,
        StatusCode::OK,
        "another tenant's checkout survives"
    );
    let audit = c.audit("checkout_origin.mode_switched").await;
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert_eq!(audit[0]["actor"], OWNER);
    assert_eq!(audit[0]["entity_id"], origin);
    assert_eq!(
        (
            audit[0]["diff"]["from"].clone(),
            audit[0]["diff"]["to"].clone()
        ),
        (json!("isolated"), json!("same_origin"))
    );

    let shop_token = c.shop_cart().await;
    let k = c.started(&shop_token, None).await;
    let (status, again) = c.set_mode(&origin, "same_origin").await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        again["mode_generation"], 2,
        "switching to the current mode is a no-op"
    );
    assert_eq!(
        c.checkout(&k).await.0,
        StatusCode::OK,
        "and revokes nothing"
    );
    assert_eq!(purged.lock().await.len(), 1, "nor purges");
    assert_eq!(c.audit("checkout_origin.mode_switched").await.len(), 1);

    let mut unready = c.s.clone();
    unready.same_origin_ready = false;
    let (status, back) = c
        .admin_on(
            &unready,
            Call::post(
                "/admin/v1/checkout-origins/mode",
                json!({ "origin": origin, "mode": "isolated" }),
            ),
            OWNER,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "rollback needs no readiness: {back}"
    );
    assert_eq!(
        (back["mode"].clone(), back["mode_generation"].clone()),
        (json!("isolated"), json!(3))
    );
    assert_eq!(
        c.checkout(&k).await.0,
        StatusCode::NOT_FOUND,
        "revoked again"
    );
}
