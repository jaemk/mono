use axum::http::StatusCode;
use axum::response::IntoResponse;
use cached::proc_macro::cached;
use serde::Deserialize;

lazy_static::lazy_static! {
    static ref CLIENT: reqwest::Client = reqwest::Client::builder()
        .user_agent("https://outside.kominick.com")
        .build()
        .expect("failed to build reqwest client");
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PointsResponse {
    properties: PointsProperties,
}

#[derive(Deserialize)]
struct PointsProperties {
    forecast: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForecastResponse {
    properties: ForecastProperties,
}

#[derive(Deserialize)]
struct ForecastProperties {
    periods: Vec<Period>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Period {
    name: String,
    temperature: i32,
    temperature_unit: String,
    wind_speed: String,
    wind_direction: String,
    detailed_forecast: String,
}

#[cached(
    time = 3600,
    result = true,
    key = "String",
    convert = r#"{ format!("{},{}", lat, long) }"#
)]
pub async fn get_weather_cached(lat: &str, long: &str) -> Result<String, String> {
    get_weather(lat, long).await.map_err(|e| e.to_string())
}

async fn get_weather(lat: &str, long: &str) -> anyhow::Result<String> {
    let points_url = format!("https://api.weather.gov/points/{},{}", lat, long);
    let points: PointsResponse = CLIENT.get(&points_url).send().await?.json().await?;

    let forecast: ForecastResponse = CLIENT
        .get(&points.properties.forecast)
        .send()
        .await?
        .json()
        .await?;

    let result = forecast
        .properties
        .periods
        .into_iter()
        .map(|period| {
            format!(
                "{}: {} {}, {} {}\n{}",
                period.name,
                period.temperature,
                period.temperature_unit,
                period.wind_speed,
                period.wind_direction,
                period.detailed_forecast
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    Ok(result)
}

pub async fn index() -> impl IntoResponse {
    match get_weather_cached("41.351691", "-71.718995").await {
        Ok(weather) => (StatusCode::OK, weather).into_response(),
        Err(e) => {
            tracing::error!("failed to get weather: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong...").into_response()
        }
    }
}
