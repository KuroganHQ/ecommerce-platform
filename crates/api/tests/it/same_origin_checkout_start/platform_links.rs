use super::*;

async fn shop_model(c: &Ctx) -> Value {
    let (status, shop, _) = c
        .edge
        .call(Call::get("/storefront/v1/shop"))
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{shop}");
    shop
}

#[sqlx::test(migrations = "../../migrations")]
async fn page_model_links_follow_the_checkout_mode(db: PgPool) {
    let c = setup(db, CheckoutMode::Isolated).await;
    let shop = shop_model(&c).await;
    assert_eq!(shop["checkout_url"], "http://checkout.shop.localhost:8080");
    assert_eq!(
        shop["withdrawal_url"],
        "http://checkout.shop.localhost:8080/withdraw"
    );
    assert_eq!(
        shop["consent"]["preferences_url"],
        "http://checkout.shop.localhost:8080/consent"
    );

    c.switch(CheckoutMode::SameOrigin).await;
    let shop = shop_model(&c).await;
    assert_eq!(
        shop["checkout_url"], "https://shop.localhost/_p/checkout/start",
        "the theme's checkout button posts the C10 start"
    );
    assert_eq!(shop["withdrawal_url"], "https://shop.localhost/withdraw");
    assert_eq!(
        shop["consent"]["preferences_url"],
        "https://shop.localhost/consent"
    );
    assert_eq!(
        shop["account_url"], "http://shop.accounts.localhost:8080",
        "accounts stay on the account origin"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn same_origin_confirmation_links_the_shop_origin(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    c.placed(method, "").await;
    let body: String = sqlx::query_scalar(
        "SELECT body_text FROM email_messages WHERE template = 'order_confirmation'",
    )
    .fetch_one(&c.db)
    .await
    .unwrap();
    assert!(
        body.contains("https://shop.localhost/o/") && !body.contains("checkout."),
        "the order link is on the shop origin: {body}"
    );
}
