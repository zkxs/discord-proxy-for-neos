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
    #[serde(rename = "FEF512C2B723BCD6360C3AA26215BC95173FD437FC149C298CE93C4B081E3F28")]
    pub unique_device_identifier_hash: Option<String>,
    pub steam: Option<String>,
    #[serde(rename = "F71A850301976CEE8F8F2E1386105374539C67EEE5F2F187C0DACA2C7426360B")]
    pub steam_hash: Option<String>,
    pub discord: Option<String>,
    #[serde(rename = "053BC65874AD6098E58C41C57B378A2F36B0220E5E0B46722245E6C2F796818C")]
    pub discord_hash: Option<String>,
}

#[derive(Deserialize)]
pub struct Entry {
    pub snowflake: u64,
    pub hash: Vec<u8>,
}
