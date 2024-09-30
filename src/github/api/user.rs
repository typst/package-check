use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub login: String,
}
