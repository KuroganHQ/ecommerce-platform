use super::*;

struct IsolatedOrder {
    cart: String,
    token: String,
    attempt: String,
}

impl Ctx {
    async fn isolated_order(&self) -> IsolatedOrder {
        let method = self.placeable().await;
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
        let (status, text, _) = self
            .place(&cart, "isolated", &super::pii_rotation::place_body(&view, ""))
            .await;
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
        assert_eq!(status, StatusCode::OK, "the order token still reads: {body}");
        body["payment"]["can_pay"].as_bool().unwrap()
    }

    async fn fake_pay_page(&self, order: &IsolatedOrder) -> StatusCode {
        self.call(
            Call::get(&format!(
                "/storefront/v1/checkout/fake-pay/{}",
                order.attempt
            )),
            &order.cart,
        )
        .await
        .0
    }
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
