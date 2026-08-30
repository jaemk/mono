//! `/flip` - a coin flip spa, plus the unlinked `/flip/odds` page that biases
//! the coin in 10% increments.
//!
//! The odds ride on a cookie rather than anything stored here, so they belong
//! to one browser and follow it to whichever machine answers. Nothing to keep
//! in sync between regions and nothing to lose on a restart.
//!
//! The cookie is http-only and the draw happens server side, so the flip page
//! never has the number in reach: the browser sends the cookie without being
//! able to read it, and gets back only a side.

use crate::{AppState, TERA};
use axum::{
    extract::Json,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use tera::Context;

/// Odds are adjustable in 10% increments.
pub const ODDS_STEP: u8 = 10;

/// A fair coin, what a browser without a usable cookie gets.
pub const DEFAULT_HEADS_PCT: u8 = 50;

/// Holds the percent of flips that should come up heads.
const ODDS_COOKIE: &str = "flip_odds";

/// Persistent rather than session scoped, so the odds survive a browser
/// restart and only go away when cookies are cleared.
const ODDS_COOKIE_DAYS: i64 = 365;

/// Odds must land on a step the `/flip/odds` slider can actually produce.
pub fn validate_heads_pct(pct: i64) -> Result<u8, String> {
    if !(0..=100).contains(&pct) {
        return Err(format!("heads_pct must be between 0 and 100, got {pct}"));
    }
    if pct % i64::from(ODDS_STEP) != 0 {
        return Err(format!(
            "heads_pct must be a multiple of {ODDS_STEP}, got {pct}"
        ));
    }
    Ok(pct as u8)
}

/// This browser's odds. Anything missing or unparseable is a fair coin, so a
/// hand edited cookie can't push the draw outside 0-100.
fn odds_from_jar(jar: &CookieJar) -> u8 {
    jar.get(ODDS_COOKIE)
        .and_then(|c| c.value().parse::<i64>().ok())
        .and_then(|pct| validate_heads_pct(pct).ok())
        .unwrap_or(DEFAULT_HEADS_PCT)
}

/// Scoped to `/flip` so it rides along with the flip requests and nothing
/// else, and http-only so the page can't read the number back out.
fn odds_cookie(pct: u8) -> Cookie<'static> {
    Cookie::build((ODDS_COOKIE, pct.to_string()))
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(ODDS_COOKIE_DAYS))
        .path("/flip")
        .build()
}

/// Draw a side. `random_ratio` draws from an integer ratio, so an untouched
/// 50/100 coin is exactly fair - no float rounding, no modulo bias, and seeded
/// from the os rng.
fn draw(heads_pct: u8) -> &'static str {
    if rand::rng().random_ratio(u32::from(heads_pct), 100) {
        "heads"
    } else {
        "tails"
    }
}

/// Serves the spa shell. `/flip` and `/flip/odds` are the same document; the
/// page picks its view from the path.
pub async fn index() -> impl IntoResponse {
    match TERA.render("flip.html", &Context::new()) {
        Ok(s) => ([(header::CONTENT_TYPE, "text/html")], s).into_response(),
        Err(e) => {
            tracing::error!("tera render error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "content error").into_response()
        }
    }
}

#[derive(Serialize)]
pub struct OddsResponse {
    pub heads_pct: u8,
    pub step: u8,
}

fn odds_response(heads_pct: u8) -> Json<OddsResponse> {
    Json(OddsResponse {
        heads_pct,
        step: ODDS_STEP,
    })
}

pub async fn get_odds(jar: CookieJar) -> impl IntoResponse {
    odds_response(odds_from_jar(&jar))
}

#[derive(Deserialize)]
pub struct SetOddsRequest {
    /// Signed and wide so out of range values are a 400 from us rather than a
    /// 422 out of the json extractor.
    pub heads_pct: i64,
}

pub async fn set_odds(jar: CookieJar, Json(req): Json<SetOddsRequest>) -> Response {
    let pct = match validate_heads_pct(req.heads_pct) {
        Ok(pct) => pct,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"code": 400, "message": message})),
            )
                .into_response();
        }
    };
    (jar.add(odds_cookie(pct)), odds_response(pct)).into_response()
}

#[derive(Serialize)]
pub struct FlipResponse {
    pub result: &'static str,
}

/// Flipping happens here rather than in the browser so the odds stay off the
/// wire on the flip page.
pub async fn flip(jar: CookieJar) -> impl IntoResponse {
    Json(FlipResponse {
        result: draw(odds_from_jar(&jar)),
    })
}

/// Full paths rather than a nested router so the trailing slash variants can
/// be spelled out - axum's `nest` only matches the un-slashed prefix.
pub fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/flip", get(index))
        .route("/flip/", get(index))
        .route("/flip/odds", get(index))
        .route("/flip/odds/", get(index))
        .route("/flip/api/odds", get(get_odds).post(set_odds))
        .route("/flip/api/flip", post(flip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_tera;

    fn jar_with(value: &str) -> CookieJar {
        CookieJar::new().add(Cookie::new(ODDS_COOKIE, value.to_string()))
    }

    /// The spa shell is one document holding both views, and the odds page is
    /// not linked from anywhere.
    #[test]
    fn test_flip_template_renders() {
        let rendered = workspace_tera()
            .render("flip.html", &Context::new())
            .expect("flip.html should render");
        assert!(rendered.contains("id=\"view-flip\""));
        assert!(rendered.contains("id=\"view-odds\""));
        assert!(rendered.contains("/flip/api/flip"));
        assert!(rendered.contains("/flip/api/odds"));
    }

    #[test]
    fn test_validate_heads_pct_accepts_steps() {
        for pct in [0, 10, 50, 90, 100] {
            assert_eq!(validate_heads_pct(pct).unwrap(), pct as u8);
        }
    }

    #[test]
    fn test_validate_heads_pct_rejects_off_step() {
        assert!(validate_heads_pct(55).is_err());
        assert!(validate_heads_pct(1).is_err());
    }

    #[test]
    fn test_validate_heads_pct_rejects_out_of_range() {
        assert!(validate_heads_pct(-10).is_err());
        assert!(validate_heads_pct(110).is_err());
        assert!(validate_heads_pct(i64::MAX).is_err());
    }

    #[test]
    fn test_no_cookie_is_a_fair_coin() {
        assert_eq!(odds_from_jar(&CookieJar::new()), DEFAULT_HEADS_PCT);
    }

    #[test]
    fn test_cookie_carries_the_odds() {
        assert_eq!(odds_from_jar(&jar_with("70")), 70);
        assert_eq!(odds_from_jar(&jar_with("0")), 0);
        assert_eq!(odds_from_jar(&jar_with("100")), 100);
    }

    /// A hand edited cookie can't push the draw off the steps or out of range,
    /// which would panic `random_ratio`.
    #[test]
    fn test_tampered_cookie_falls_back_to_fair() {
        for value in [
            "",
            "abc",
            "55",
            "-10",
            "110",
            "1e3",
            "9999999999999999999999",
        ] {
            assert_eq!(
                odds_from_jar(&jar_with(value)),
                DEFAULT_HEADS_PCT,
                "cookie value {value:?} should fall back to a fair coin"
            );
        }
    }

    #[test]
    fn test_odds_cookie_is_persistent_and_unreadable() {
        let cookie = odds_cookie(80);
        assert_eq!(cookie.name(), ODDS_COOKIE);
        assert_eq!(cookie.value(), "80");
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
        assert_eq!(cookie.path(), Some("/flip"));
        assert_eq!(cookie.max_age(), Some(time::Duration::days(365)));
    }

    /// What the cookie stores is what a later request reads back out.
    #[test]
    fn test_cookie_round_trips() {
        for pct in [0, 10, 50, 90, 100] {
            let cookie = odds_cookie(pct);
            let jar = CookieJar::new().add(cookie);
            assert_eq!(odds_from_jar(&jar), pct);
        }
    }

    #[test]
    fn test_draw_respects_deterministic_odds() {
        for _ in 0..100 {
            assert_eq!(draw(100), "heads");
            assert_eq!(draw(0), "tails");
        }
    }

    /// A fair coin over 20k draws lands within ~7 standard deviations of even.
    #[test]
    fn test_fair_coin_is_balanced() {
        let heads = (0..20_000).filter(|_| draw(50) == "heads").count();
        assert!(
            (9_500..=10_500).contains(&heads),
            "expected a roughly even split, got {heads} heads out of 20000"
        );
    }

    /// A weighted coin tracks the configured odds just as tightly.
    #[test]
    fn test_weighted_coin_tracks_odds() {
        let heads = (0..20_000).filter(|_| draw(70) == "heads").count();
        assert!(
            (13_500..=14_500).contains(&heads),
            "expected ~70% heads, got {heads} heads out of 20000"
        );
    }
}
