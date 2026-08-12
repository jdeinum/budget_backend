pub mod app;
pub mod config;
mod error;
mod plaid;
mod repo;
mod routes;
mod state;
mod statement;
mod sync;
mod utils;

pub use app::build;
