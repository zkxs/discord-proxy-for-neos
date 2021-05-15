use serde::Deserialize;

#[derive(Deserialize)]
pub struct User {
    pub username: String,
    pub discriminator: String,
}

impl User {
    pub fn as_pretty_string(&self) -> String {
        format!("{} #{}", self.username, self.discriminator)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExtraUserIds {
    pub unique_device_identifier: Option<String>,
    pub steam: Option<String>,
    pub discord: Option<String>,
}
