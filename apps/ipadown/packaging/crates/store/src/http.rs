use crate::config::USER_AGENT;
use crate::error::Result;

pub fn client_follow() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().user_agent(USER_AGENT).build()?)
}
