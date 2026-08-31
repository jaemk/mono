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
//!
//! The coin's own look - what the two sides are called and what colors they
//! are - rides on a second cookie of its own, and is rendered into the page
//! server side so it arrives already customized.

use crate::{AppState, TERA};
use axum::{
    extract::Json,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::Engine as _;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use tera::Context;

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Odds are adjustable in 10% increments.
pub const ODDS_STEP: u8 = 10;

/// A fair coin, what a browser without a usable cookie gets.
pub const DEFAULT_HEADS_PCT: u8 = 50;

/// Holds the percent of flips that should come up heads.
const ODDS_COOKIE: &str = "flip_odds";

/// Holds the coin's names and colors.
const COIN_COOKIE: &str = "flip_coin";

/// Persistent rather than session scoped, so the cookies survive a browser
/// restart and only go away when cookies are cleared.
const ODDS_COOKIE_DAYS: i64 = 365;

/// Long enough to keep a face readable on the coin.
pub const MAX_NAME_CHARS: usize = 16;

const DEFAULT_HEADS_NAME: &str = "heads";
const DEFAULT_TAILS_NAME: &str = "tails";
const DEFAULT_HEADS_COLOR: &str = "#e0b354";
const DEFAULT_TAILS_COLOR: &str = "#b9c2cc";

/// Face text for a light coin and for a dark one.
const INK_DARK: &str = "#241a04";
const INK_LIGHT: &str = "#f4f2ef";

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

/// Scoped to `/flip` so these ride along with the flip requests and nothing
/// else, and http-only so the page can't read the odds back out.
fn build_cookie(name: &'static str, value: String) -> Cookie<'static> {
    Cookie::build((name, value))
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(ODDS_COOKIE_DAYS))
        .path("/flip")
        .build()
}

fn odds_cookie(pct: u8) -> Cookie<'static> {
    build_cookie(ODDS_COOKIE, pct.to_string())
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

// ---------------------------------------------------------------------------
// The coin's look
// ---------------------------------------------------------------------------

/// One side of the coin as it is stored in the cookie.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Face {
    pub name: String,
    pub color: String,
}

/// Both sides. Serialized into the coin cookie, so field names are part of
/// the stored format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coin {
    pub heads: Face,
    pub tails: Face,
}

impl Default for Coin {
    fn default() -> Self {
        Self {
            heads: Face {
                name: DEFAULT_HEADS_NAME.to_string(),
                color: DEFAULT_HEADS_COLOR.to_string(),
            },
            tails: Face {
                name: DEFAULT_TAILS_NAME.to_string(),
                color: DEFAULT_TAILS_COLOR.to_string(),
            },
        }
    }
}

impl Coin {
    /// Replace any field that isn't usable with its default, so a stale or
    /// hand edited cookie degrades one field at a time instead of throwing
    /// the whole coin away.
    fn sanitized(self) -> Self {
        let fallback = Self::default();
        Self {
            heads: Face {
                name: validate_name(&self.heads.name).unwrap_or(fallback.heads.name),
                color: validate_color(&self.heads.color).unwrap_or(fallback.heads.color),
            },
            tails: Face {
                name: validate_name(&self.tails.name).unwrap_or(fallback.tails.name),
                color: validate_color(&self.tails.color).unwrap_or(fallback.tails.color),
            },
        }
    }

    /// What the template and the api hand out: the stored fields plus the
    /// face text color that goes with each background.
    fn view(&self) -> CoinView {
        CoinView {
            heads: FaceView {
                name: self.heads.name.clone(),
                color: self.heads.color.clone(),
                ink: ink_for(&self.heads.color),
            },
            tails: FaceView {
                name: self.tails.name.clone(),
                color: self.tails.color.clone(),
                ink: ink_for(&self.tails.color),
            },
        }
    }
}

#[derive(Serialize)]
pub struct FaceView {
    pub name: String,
    pub color: String,
    pub ink: &'static str,
}

#[derive(Serialize)]
pub struct CoinView {
    pub heads: FaceView,
    pub tails: FaceView,
}

/// Names are rendered as text, so the only limits are that there is something
/// there, it stays short enough to fit a face, and it can't smuggle in control
/// characters.
pub fn validate_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("name must not be blank".to_string());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("name must be at most {MAX_NAME_CHARS} characters"));
    }
    if name.chars().any(char::is_control) {
        return Err("name must not contain control characters".to_string());
    }
    Ok(name.to_string())
}

/// Colors land in a stylesheet, where html escaping would not save us, so
/// only a literal `#rrggbb` is allowed through and it is normalized on the
/// way in.
pub fn validate_color(raw: &str) -> Result<String, String> {
    let color = raw.trim();
    let hex = color
        .strip_prefix('#')
        .ok_or_else(|| format!("color must look like #rrggbb, got {color:?}"))?;
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("color must look like #rrggbb, got {color:?}"));
    }
    Ok(format!("#{}", hex.to_ascii_lowercase()))
}

/// Dark text on a light face, light text on a dark one. Relative luminance
/// per wcag: linearize each channel, then weight it.
///
/// The page mirrors this in `inkFor` so a color picked with the picker reads
/// correctly before the save comes back.
fn ink_for(color: &str) -> &'static str {
    let channel = |offset: usize| -> f64 {
        let raw = u8::from_str_radix(&color[offset..offset + 2], 16).unwrap_or(0);
        let c = f64::from(raw) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    // validated upstream, but a short value must not panic the slice
    if color.len() != 7 {
        return INK_DARK;
    }
    let luminance = 0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5);
    if luminance > 0.4 {
        INK_DARK
    } else {
        INK_LIGHT
    }
}

/// This browser's coin. Anything missing or unreadable is the default coin.
fn coin_from_jar(jar: &CookieJar) -> Coin {
    jar.get(COIN_COOKIE)
        .and_then(|c| B64.decode(c.value()).ok())
        .and_then(|raw| serde_json::from_slice::<Coin>(&raw).ok())
        .map(Coin::sanitized)
        .unwrap_or_default()
}

/// Base64 so the json survives the cookie value charset intact.
fn coin_cookie(coin: &Coin) -> Cookie<'static> {
    let encoded = B64.encode(serde_json::to_vec(coin).expect("a coin is always serializable"));
    build_cookie(COIN_COOKIE, encoded)
}

/// Serves the spa shell. `/flip` and `/flip/odds` are the same document; the
/// page picks its view from the path. The coin is rendered in rather than
/// fetched, so a customized coin is right on the first paint.
pub async fn index(jar: CookieJar) -> impl IntoResponse {
    let mut context = Context::new();
    context.insert("coin", &coin_from_jar(&jar).view());
    match TERA.render("flip.html", &context) {
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

#[derive(Deserialize)]
pub struct SetCoinRequest {
    pub heads_name: String,
    pub tails_name: String,
    pub heads_color: String,
    pub tails_color: String,
}

impl SetCoinRequest {
    /// All four fields or none, so a rejected edit never leaves the coin half
    /// changed.
    fn into_coin(self) -> Result<Coin, String> {
        Ok(Coin {
            heads: Face {
                name: validate_name(&self.heads_name).map_err(|e| format!("heads {e}"))?,
                color: validate_color(&self.heads_color).map_err(|e| format!("heads {e}"))?,
            },
            tails: Face {
                name: validate_name(&self.tails_name).map_err(|e| format!("tails {e}"))?,
                color: validate_color(&self.tails_color).map_err(|e| format!("tails {e}"))?,
            },
        })
    }
}

pub async fn set_coin(jar: CookieJar, Json(req): Json<SetCoinRequest>) -> Response {
    let coin = match req.into_coin() {
        Ok(coin) => coin,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"code": 400, "message": message})),
            )
                .into_response();
        }
    };
    (jar.add(coin_cookie(&coin)), Json(coin.view())).into_response()
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
        .route("/flip/api/coin", post(set_coin))
        .route("/flip/api/flip", post(flip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_tera;

    fn jar_with(value: &str) -> CookieJar {
        CookieJar::new().add(Cookie::new(ODDS_COOKIE, value.to_string()))
    }

    fn coin_jar_with(value: &str) -> CookieJar {
        CookieJar::new().add(Cookie::new(COIN_COOKIE, value.to_string()))
    }

    fn render(coin: &Coin) -> String {
        let mut context = Context::new();
        context.insert("coin", &coin.view());
        workspace_tera()
            .render("flip.html", &context)
            .expect("flip.html should render")
    }

    fn custom_coin() -> Coin {
        Coin {
            heads: Face {
                name: "yes".to_string(),
                color: "#112233".to_string(),
            },
            tails: Face {
                name: "no".to_string(),
                color: "#ffee00".to_string(),
            },
        }
    }

    /// The spa shell is one document holding both views, and the odds page is
    /// not linked from anywhere.
    #[test]
    fn test_flip_template_renders() {
        let rendered = render(&Coin::default());
        assert!(rendered.contains("id=\"view-flip\""));
        assert!(rendered.contains("id=\"view-odds\""));
        assert!(rendered.contains("/flip/api/flip"));
        assert!(rendered.contains("/flip/api/odds"));
        assert!(rendered.contains("/flip/api/coin"));
    }

    /// The coin arrives already customized: names on both faces and on the
    /// odds labels, colors in the stylesheet.
    #[test]
    fn test_template_renders_the_customized_coin() {
        let rendered = render(&custom_coin());
        assert!(rendered.contains("--heads-bg: #112233;"));
        assert!(rendered.contains("--tails-bg: #ffee00;"));
        // a dark face takes light ink, a bright one takes dark ink
        assert!(rendered.contains(&format!("--heads-ink: {INK_LIGHT};")));
        assert!(rendered.contains(&format!("--tails-ink: {INK_DARK};")));
        assert!(rendered.contains("id=\"face-heads\">yes<"));
        assert!(rendered.contains("id=\"face-tails\">no<"));
        assert!(rendered.contains("id=\"odds-heads-label\">yes<"));
        assert!(rendered.contains("id=\"odds-tails-label\">no<"));
    }

    /// Names are rendered as text and into attributes, so tera's autoescaping
    /// has to hold for a name that looks like markup.
    #[test]
    fn test_template_escapes_names() {
        let coin = Coin {
            heads: Face {
                name: "<script>x".to_string(),
                color: "#112233".to_string(),
            },
            ..Coin::default()
        };
        let rendered = render(&coin);
        assert!(!rendered.contains("<script>x"));
        assert!(rendered.contains("&lt;script&gt;x"));
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

    // ---- the coin's look ---------------------------------------------------

    #[test]
    fn test_validate_name_trims_and_accepts() {
        assert_eq!(validate_name("yes").unwrap(), "yes");
        assert_eq!(validate_name("  spaced out  ").unwrap(), "spaced out");
        assert_eq!(
            validate_name("sixteen chars ok").unwrap(),
            "sixteen chars ok"
        );
        // counted in characters, not bytes
        assert_eq!(validate_name("aaaaaaaaaaaaaaaa").unwrap().len(), 16);
        assert!(validate_name("ünïcödé").is_ok());
    }

    #[test]
    fn test_validate_name_rejects_blank_long_and_control() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("seventeen chars!!").is_err());
        assert!(validate_name("new\nline").is_err());
        assert!(validate_name("null\0byte").is_err());
    }

    #[test]
    fn test_validate_color_normalizes() {
        assert_eq!(validate_color("#AABBCC").unwrap(), "#aabbcc");
        assert_eq!(validate_color("  #ffee00 ").unwrap(), "#ffee00");
    }

    #[test]
    fn test_validate_color_rejects_anything_else() {
        // css that would otherwise land in the stylesheet as written
        assert!(validate_color("red").is_err());
        assert!(validate_color("#fff").is_err());
        assert!(validate_color("#gggggg").is_err());
        assert!(validate_color("#aabbccdd").is_err());
        assert!(validate_color("").is_err());
        assert!(validate_color("#aabbcc; } body { display: none").is_err());
    }

    #[test]
    fn test_ink_follows_the_background() {
        assert_eq!(ink_for("#ffffff"), INK_DARK);
        assert_eq!(ink_for("#000000"), INK_LIGHT);
        assert_eq!(ink_for("#1a2b6d"), INK_LIGHT);
        // both defaults are light faces
        assert_eq!(ink_for(DEFAULT_HEADS_COLOR), INK_DARK);
        assert_eq!(ink_for(DEFAULT_TAILS_COLOR), INK_DARK);
    }

    #[test]
    fn test_no_cookie_is_the_default_coin() {
        assert_eq!(coin_from_jar(&CookieJar::new()), Coin::default());
        let coin = Coin::default();
        assert_eq!(coin.heads.name, "heads");
        assert_eq!(coin.tails.name, "tails");
    }

    #[test]
    fn test_coin_cookie_round_trips() {
        let coin = custom_coin();
        let jar = CookieJar::new().add(coin_cookie(&coin));
        assert_eq!(coin_from_jar(&jar), coin);
    }

    #[test]
    fn test_coin_cookie_matches_the_odds_cookie_attributes() {
        let cookie = coin_cookie(&Coin::default());
        assert_eq!(cookie.name(), COIN_COOKIE);
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
        assert_eq!(cookie.path(), Some("/flip"));
        assert_eq!(cookie.max_age(), Some(time::Duration::days(365)));
    }

    #[test]
    fn test_unreadable_coin_cookie_falls_back() {
        for value in ["", "not base64!!", "YWJj", "e30"] {
            assert_eq!(
                coin_from_jar(&coin_jar_with(value)),
                Coin::default(),
                "cookie value {value:?} should fall back to the default coin"
            );
        }
    }

    /// A cookie carrying one bad field keeps the good ones.
    #[test]
    fn test_coin_cookie_is_sanitized_field_by_field() {
        let stored = Coin {
            heads: Face {
                name: "keeper".to_string(),
                color: "red; } body { display: none".to_string(),
            },
            tails: Face {
                name: "   ".to_string(),
                color: "#ffee00".to_string(),
            },
        };
        let encoded = B64.encode(serde_json::to_vec(&stored).unwrap());
        let coin = coin_from_jar(&coin_jar_with(&encoded));

        assert_eq!(coin.heads.name, "keeper");
        assert_eq!(coin.heads.color, DEFAULT_HEADS_COLOR);
        assert_eq!(coin.tails.name, DEFAULT_TAILS_NAME);
        assert_eq!(coin.tails.color, "#ffee00");
    }

    #[test]
    fn test_set_coin_request_takes_all_four_or_none() {
        let ok = SetCoinRequest {
            heads_name: " yes ".to_string(),
            tails_name: "no".to_string(),
            heads_color: "#112233".to_string(),
            tails_color: "#FFEE00".to_string(),
        }
        .into_coin()
        .unwrap();
        assert_eq!(ok, custom_coin());

        let bad = SetCoinRequest {
            heads_name: "yes".to_string(),
            tails_name: "no".to_string(),
            heads_color: "#112233".to_string(),
            tails_color: "chartreuse".to_string(),
        }
        .into_coin();
        assert!(bad.unwrap_err().starts_with("tails "));
    }
}
