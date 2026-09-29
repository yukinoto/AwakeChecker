use crate::{db::Db, live::Live};

#[derive(Clone)]
pub struct App {
    pub db: Db,
    pub live: Live,
}
