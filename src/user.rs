use serde::Deserialize;

#[derive(Deserialize)]
pub struct User {
    pub username: String,
    pub discriminator: String,
}

impl User {
    pub fn into_pretty_string(self) -> String {
        format!("{} #{}", self.username, self.discriminator)
    }
}
