pub mod auth;
pub mod config;
pub mod handlers;
pub mod models;
pub mod service;
pub mod storage;
pub mod test_utils;

pub use config::Config;
use std::sync::Arc;

pub type State = Arc<Resources>;

pub struct Resources {
    pub db: common::db::DbPool,
    pub config: Config,
    pub s3: aws_sdk_s3::Client,
}

impl Resources {
    pub fn new(db: common::db::DbPool, config: Config, s3: aws_sdk_s3::Client) -> Self {
        Self { db, config, s3 }
    }
}
