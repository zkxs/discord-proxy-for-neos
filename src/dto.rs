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
    // "UniqueDeviceIdentifier"
    pub unique_device_identifier: Option<String>,
    #[serde(rename = "2752D89C88F1B3689A6BDA82917570E5F7AB6303D15A4A19F05CE4D31A9506C6")]
    pub unique_device_identifier_hash: Option<String>,

    // "Steam"
    pub steam: Option<String>,
    #[serde(rename = "98750D586F1C412FDA7859E83F19DE802537CB92AD741C720D6AA38F61329278")]
    pub steam_hash: Option<String>,

    // "Discord"
    pub discord: Option<String>,
    #[serde(rename = "64A8ED529D50BF94F38F81A48284C5B1981752F4E7956529947C93EE470C8DF0")]
    pub discord_hash: Option<String>,
}

#[derive(Deserialize)]
pub struct Entry {
    pub snowflake: u64,
    pub hash: Vec<u8>,
}
