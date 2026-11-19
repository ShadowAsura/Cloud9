use crate::Error;
use md5::{Digest, Md5};
use reqwest::{blocking::Client, redirect};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    thread,
    time::{Duration, Instant},
};

const OAUTH: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0";
const SERVICES: &str = "https://api.minecraftservices.com";

/// A game session. Credentials remain in memory and are omitted from diagnostic output.
pub struct Account {
    pub name: String,
    pub uuid: String,
    pub(crate) token: String,
    pub(crate) kind: &'static str,
}

impl Account {
    pub fn local_dev(name: &str) -> Result<Self, Error> {
        if !valid_name(name) {
            return Err(Error::Config(
                "Dev username must contain 1 to 16 ASCII letters, digits, or underscores".into(),
            ));
        }
        let mut digest = Md5::digest(format!("OfflinePlayer:{name}"));
        digest[6] = (digest[6] & 0x0f) | 0x30;
        digest[8] = (digest[8] & 0x3f) | 0x80;
        Ok(Self {
            name: name.to_owned(),
            uuid: format!("{digest:x}"),
            token: "0".into(),
            kind: "legacy",
        })
    }

    pub fn authenticated(&self) -> bool {
        self.kind == "msa"
    }
}

fn valid_name(name: &str) -> bool {
    (1..=16).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[derive(Deserialize)]
struct DeviceResponse {
    verification_uri: String,
    user_code: String,
    device_code: String,
    expires_in: u64,
    interval: u64,
}

pub struct MicrosoftSignIn {
    http: Client,
    client_id: String,
    device: DeviceResponse,
    expires: Instant,
}

impl MicrosoftSignIn {
    /// Use an authorized Microsoft public-client application ID.
    pub fn begin(client_id: &str) -> Result<Self, Error> {
        if client_id.len() != 36
            || !client_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err(Error::Config(
                "enter a Microsoft application (client) ID".into(),
            ));
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(redirect::Policy::none())
            .build()?;
        let device: DeviceResponse = http
            .post(format!("{OAUTH}/devicecode"))
            .form(&[("client_id", client_id), ("scope", "XboxLive.signin")])
            .send()?
            .error_for_status()?
            .json()?;
        let expires = Instant::now() + Duration::from_secs(device.expires_in.min(3600));
        Ok(Self {
            http,
            client_id: client_id.to_owned(),
            device,
            expires,
        })
    }

    pub fn verification(&self) -> (&str, &str) {
        (&self.device.verification_uri, &self.device.user_code)
    }

    /// Poll at Microsoft's interval and allow the calling UI to cancel.
    pub fn finish(self, cancelled: impl Fn() -> bool) -> Result<Account, Error> {
        let mut interval = Duration::from_secs(self.device.interval.clamp(5, 60));
        while Instant::now() < self.expires && !cancelled() {
            let until = Instant::now() + interval;
            while Instant::now() < until && Instant::now() < self.expires && !cancelled() {
                thread::sleep(Duration::from_millis(200));
            }
            if cancelled() || Instant::now() >= self.expires {
                break;
            }
            let response = self
                .http
                .post(format!("{OAUTH}/token"))
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", self.device.device_code.as_str()),
                ])
                .send()?;
            let successful = response.status().is_success();
            let result: Value = response.json()?;
            if successful {
                return self.minecraft(field(&result, "access_token")?);
            }
            match result["error"].as_str() {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += Duration::from_secs(5),
                Some("authorization_declined") => {
                    return Err(Error::Config("Microsoft sign-in was declined".into()));
                }
                _ => {
                    return Err(Error::Config(
                        "Microsoft sign-in failed. Verify the application registration.".into(),
                    ));
                }
            }
        }
        Err(Error::Config(
            "Microsoft sign-in expired or was cancelled".into(),
        ))
    }

    fn minecraft(&self, microsoft_token: &str) -> Result<Account, Error> {
        let xbl: Value = self.http.post("https://user.auth.xboxlive.com/user/authenticate").json(&json!({
            "Properties": {"AuthMethod":"RPS","SiteName":"user.auth.xboxlive.com","RpsTicket":format!("d={microsoft_token}")},
            "RelyingParty":"http://auth.xboxlive.com","TokenType":"JWT"
        })).send()?.error_for_status()?.json()?;
        let response = self
            .http
            .post("https://xsts.auth.xboxlive.com/xsts/authorize")
            .json(&json!({
                "Properties":{"SandboxId":"RETAIL","UserTokens":[field(&xbl,"Token")?]},
                "RelyingParty":"rp://api.minecraftservices.com/","TokenType":"JWT"
            }))
            .send()?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Config(format!(
                "Xbox authorization failed ({status}). Check the account's Xbox profile and permissions."
            )));
        }
        let xsts: Value = response.json()?;
        let uhs = field(&xsts["DisplayClaims"]["xui"][0], "uhs")?;
        let token = field(&xsts, "Token")?;
        let response = self
            .http
            .post(format!("{SERVICES}/authentication/login_with_xbox"))
            .json(&json!({"identityToken":format!("XBL3.0 x={uhs};{token}")}))
            .send()?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Config(format!(
                "Minecraft sign-in failed ({status}). Verify the client ID is approved for Minecraft services."
            )));
        }
        let response: Value = response.json()?;
        let token = field(&response, "access_token")?;
        let profile: Value = self
            .http
            .get(format!("{SERVICES}/minecraft/profile"))
            .bearer_auth(token)
            .send()?
            .error_for_status()?
            .json()?;
        let name = field(&profile, "name")?;
        let uuid = field(&profile, "id")?;
        if !valid_name(name)
            || uuid.len() != 32
            || !uuid.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::Config(
                "Minecraft returned an invalid profile".into(),
            ));
        }
        Ok(Account {
            name: name.to_owned(),
            uuid: uuid.to_owned(),
            token: token.to_owned(),
            kind: "msa",
        })
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value[key]
        .as_str()
        .ok_or_else(|| Error::Config(format!("authentication response missing {key}")))
}
