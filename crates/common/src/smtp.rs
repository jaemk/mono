use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Message, Tokio1Executor};

#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from_email: String,
}

impl SmtpConfig {
    pub fn from_env() -> std::result::Result<Self, String> {
        let host = std::env::var("SMTP_HOST").map_err(|_| "SMTP_HOST not set".to_string())?;
        let port = std::env::var("SMTP_PORT")
            .unwrap_or_else(|_| "587".to_string())
            .parse::<u16>()
            .map_err(|_| "Invalid SMTP_PORT".to_string())?;
        let username = std::env::var("SMTP_USERNAME").ok();
        let password = std::env::var("SMTP_PASSWORD").ok();
        match (&username, &password) {
            (Some(_), None) => return Err("SMTP_USERNAME set but SMTP_PASSWORD missing".into()),
            (None, Some(_)) => return Err("SMTP_PASSWORD set but SMTP_USERNAME missing".into()),
            _ => {}
        }
        let from_email =
            std::env::var("FROM_EMAIL").map_err(|_| "FROM_EMAIL not set".to_string())?;
        Ok(Self {
            host,
            port,
            username,
            password,
            from_email,
        })
    }
}

pub async fn send_email(
    config: &SmtpConfig,
    to: &str,
    subject: &str,
    body: &str,
) -> crate::Result<()> {
    let email = Message::builder()
        .from(config.from_email.parse()?)
        .to(to.parse()?)
        .subject(subject)
        .body(body.to_string())?;

    let creds = config
        .username
        .as_ref()
        .zip(config.password.as_ref())
        .map(|(u, p)| Credentials::new(u.clone(), p.clone()));

    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)
        .map_err(|e| format!("smtp relay: {e}"))?
        .port(config.port);

    if let Some(c) = creds {
        builder = builder.credentials(c);
    }

    builder
        .build()
        .send(email)
        .await
        .map_err(|e| format!("smtp send: {e}"))?;
    Ok(())
}
