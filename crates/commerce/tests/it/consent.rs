//! Theme-callable consent (C9): anonymous cookie categories only, under a server-owned source.

use commerce::consent::{self, ConsentChoice, CookieChoice, Purposes, Source, Subject};
use platform::db::tenant_tx;
use sqlx::PgPool;

#[test]
fn a_cookie_choice_ignores_the_source_and_refuses_email_purposes() {
    for source in ["banner", "preferences", "linked", "checkout", "account"] {
        let choice: CookieChoice = serde_json::from_value(serde_json::json!({
            "purposes": {"analytics": true}, "text_version": "2026-09-25", "source": source,
        }))
        .unwrap();
        assert_eq!(choice.purposes.analytics, Some(true), "{source}");
    }
    for purpose in ["email_marketing", "review_invites", "tracking"] {
        assert!(
            serde_json::from_value::<CookieChoice>(serde_json::json!({
                "purposes": {"analytics": true, purpose: true}, "text_version": "2026-09-25",
            }))
            .is_err(),
            "{purpose}"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn anonymous_choices_carry_the_server_source_and_show_cookie_categories_only(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = testkit::storefront::shop(&runtime, "consent").await;
    let anon = consent::new_anon_id();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    consent::record(
        &mut tx,
        &Subject::Anon(anon.clone()),
        &ConsentChoice {
            purposes: Purposes {
                email_marketing: Some(true),
                ..Purposes::default()
            },
            text_version: "2026-09-25".into(),
            source: Source::Preferences,
        },
        None,
    )
    .await
    .unwrap();
    let choice: CookieChoice = serde_json::from_value(serde_json::json!({
        "purposes": {"analytics": true, "ads": false}, "text_version": "2026-10-16",
        "source": "linked",
    }))
    .unwrap();
    consent::record_anonymous(&mut tx, &anon, &choice, None)
        .await
        .unwrap();

    let state = consent::anonymous_state(&mut tx, &anon).await.unwrap();
    assert_eq!(
        state.purposes,
        Purposes {
            analytics: Some(true),
            ads: Some(false),
            ..Purposes::default()
        },
        "an email purpose recorded before C9 is not the browser's"
    );
    assert_eq!(consent::summary(&state).as_deref(), Some("analytics"));
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT subject_type, purpose, source FROM consent_records
         WHERE source <> 'preferences' ORDER BY purpose",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            ("anon".into(), "ads".into(), "browser".into()),
            ("anon".into(), "analytics".into(), "browser".into()),
        ]
    );

    let nothing: CookieChoice = serde_json::from_value(serde_json::json!({
        "purposes": {}, "text_version": "2026-10-16",
    }))
    .unwrap();
    assert!(
        consent::record_anonymous(&mut tx, &anon, &nothing, None)
            .await
            .is_err()
    );
}
