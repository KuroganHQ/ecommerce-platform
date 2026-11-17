use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use commerce::consent::{self, ConsentState, CookieChoice, Purposes, Subject};
use platform::Error;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::customer::{CONSENT_SUBJECT_HEADER, header_str, ip_hash, no_store};
use super::{Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::parse_json;

pub const SUMMARY_HEADER: &str = "x-consent-summary";
pub const EXPERIMENTS_HEADER: &str = "x-consent-experiments";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_consent, post_consent))
}

/// The consent subject (documentation only).
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
pub(crate) struct ConsentHeaders {
    /// Anonymous subject id from the consent cookie (32 hex characters).
    #[param(rename = "X-Consent-Subject")]
    x_consent_subject: Option<String>,
    /// The client's IP (stored as a salted hash with the record).
    #[param(rename = "X-Client-Ip")]
    x_client_ip: Option<String>,
    /// The browser's user agent (ad platforms that require it, WP20).
    #[param(rename = "X-Client-User-Agent")]
    x_client_user_agent: Option<String>,
}

fn anon(headers: &HeaderMap) -> Option<String> {
    header_str(headers, CONSENT_SUBJECT_HEADER)
        .filter(|s| consent::well_formed_anon(s))
        .map(str::to_owned)
}

/// The anonymous subject's cookie categories. `text_version: null` means no choice yet; the
/// email purposes are always `null`.
#[utoipa::path(
    get,
    path = "/storefront/v1/consent",
    tag = "storefront",
    params(StorefrontHeaders, ConsentHeaders),
    responses((status = 200, body = ConsentState))
)]
async fn get_consent(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let anon = anon(&headers);
    let state = with_ctx(&s, &shopper, async |tx, _| match anon {
        Some(a) => consent::cookie_state(tx, &a).await,
        None => Ok(ConsentState {
            purposes: Purposes::default(),
            text_version: None,
        }),
    })
    .await?;
    Ok(no_store(Json(state).into_response()))
}

/// Records the browser's cookie categories for the anonymous subject (minted on the first
/// choice, source `browser`) and answers its new state. A body `source` is ignored; email
/// purposes are refused.
#[utoipa::path(
    post,
    path = "/storefront/v1/consent",
    tag = "storefront",
    params(StorefrontHeaders, ConsentHeaders),
    request_body = CookieChoice,
    responses(
        (status = 200, body = ConsentState, headers(
            ("X-Consent-Subject" = String, description = "The anonymous subject id (for the edge's cookie)"),
            ("X-Consent-Summary" = String, description = "Granted purposes, comma-separated, for the script-readable `consent` cookie"),
            ("X-Consent-Experiments" = String, description = "`1` when the subject's analytics grant covers A/B experiments (the edge's `__Secure-consent_exp` cookie); absent otherwise"),
        )),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn post_consent(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let choice: CookieChoice = parse_json(&body)?;
    let subject = anon(&headers).unwrap_or_else(consent::new_anon_id);
    let (state, experiments) = with_ctx(&s, &shopper, async |tx, _| {
        let ip = ip_hash(tx, &headers).await?;
        consent::record_anonymous(tx, &subject, &choice, ip.as_deref()).await?;
        Ok((
            consent::cookie_state(tx, &subject).await?,
            consent::analytics_covers_experiments(tx, &Subject::Anon(subject.clone())).await?,
        ))
    })
    .await?;
    let mut res = Json(&state).into_response();
    let h = res.headers_mut();
    h.insert(
        CONSENT_SUBJECT_HEADER,
        HeaderValue::from_str(&subject).map_err(|e| Error::Internal(e.to_string()))?,
    );
    if let Some(summary) = consent::summary(&state) {
        h.insert(
            SUMMARY_HEADER,
            HeaderValue::from_str(&summary).map_err(|e| Error::Internal(e.to_string()))?,
        );
    }
    if experiments {
        h.insert(EXPERIMENTS_HEADER, HeaderValue::from_static("1"));
    }
    Ok(no_store(res))
}
