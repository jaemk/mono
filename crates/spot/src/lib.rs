pub mod config;
pub mod models;
pub mod service;
pub mod spotify;

pub use config::Config;
use std::sync::Arc;

pub struct SpotResources {
    pub pool: sqlx::PgPool,
    pub config: Config,
}

pub type SpotState = Arc<SpotResources>;

#[derive(Debug)]
pub struct StringError(pub String);
impl std::fmt::Display for StringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error: {}", self.0)
    }
}
impl std::error::Error for StringError {}

#[derive(Debug)]
pub struct UserAccessRevokedError;
impl std::error::Error for UserAccessRevokedError {}
impl std::fmt::Display for UserAccessRevokedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "user access revoked")
    }
}

// build a string error
#[macro_export]
macro_rules! se {
    ($($arg:tt)*) => {{ $crate::spot::StringError(format!($($arg)*))}};
}
