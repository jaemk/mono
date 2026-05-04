pub mod config;
pub mod handlers;
pub mod models;
pub mod service;
pub mod storage;
pub mod test_utils;

pub use config::Config;
use std::sync::Arc;
use tera::Tera;

pub type State = Arc<Resources>;

pub struct Resources {
    pub tera: Tera,
    pub db: common::db::DbPool,
    pub config: Config,
    pub s3: aws_sdk_s3::Client,
    /// Channel used to enqueue pastes for background deletion.
    pub deletion_tx: tokio::sync::mpsc::Sender<models::DeletionRequest>,
}

impl Resources {
    pub fn new(
        tera: Tera,
        db: common::db::DbPool,
        config: Config,
        s3: aws_sdk_s3::Client,
        deletion_tx: tokio::sync::mpsc::Sender<models::DeletionRequest>,
    ) -> Self {
        Self {
            tera,
            db,
            config,
            s3,
            deletion_tx,
        }
    }
}
