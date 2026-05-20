pub mod config;
pub mod handlers;
pub mod models;
pub mod service;
pub mod storage;
pub mod sweep;
pub mod test_utils;

pub use config::Config;
use std::sync::Arc;

pub type State = Arc<Resources>;

pub struct Resources {
    pub db: common::db::DbPool,
    pub s3: aws_sdk_s3::Client,
    pub config: Config,
}
