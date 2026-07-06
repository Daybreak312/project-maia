mod ingest;
mod search;
mod documents;
pub mod settings;

pub use ingest::ingest_handler;
pub use search::search_handler;
pub use documents::{get_document_handler, recent_handler, update_document_handler, delete_document_handler, reindex_handler};
