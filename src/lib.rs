pub mod app;
pub mod config;
mod cursor;
mod db_uuid;
mod error;
mod plaid;
mod repo;
mod routes;
mod search;
mod source;
mod state;
mod statement;
mod sync;

pub use app::build;
