use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Debug)]
pub struct DiscordHttpError(pub u16);
impl std::fmt::Display for DiscordHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Discord API rejected request (HTTP {}); check bot permissions and setup",
            self.0
        )
    }
}
impl std::error::Error for DiscordHttpError {}
/// Keep credentials only in the host transport; errors never include URLs or response bodies.
#[derive(Clone)]
pub struct DiscordRest {
    agent: ureq::Agent,
    token: String,
    base: String,
    bot_id: String,
}
impl DiscordRest {
    pub fn new(token: String) -> Result<Self> {
        if token.trim().is_empty() {
            bail!("Discord bot credential is missing from Keychain");
        }
        Ok(Self {
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(30))
                .redirects(0)
                .build(),
            token,
            base: "https://discord.com/api/v10".into(),
            bot_id: String::new(),
        })
    }
    #[doc(hidden)]
    pub fn with_test_endpoint(token: String, endpoint: String) -> Result<Self> {
        if !endpoint.starts_with("http://127.0.0.1:") {
            bail!("fixture endpoint must be loopback");
        }
        let mut client = Self::new(token)?;
        client.base = endpoint;
        Ok(client)
    }
    pub fn request(&self, method: &str, route: &str, body: Option<Value>) -> Result<Value> {
        for _ in 0..4 {
            let request = self
                .agent
                .request(method, &format!("{}{route}", self.base))
                .set("Authorization", &format!("Bot {}", self.token))
                .set("User-Agent", "DiscordBot (https://fluent.computer, 0.6)");
            let response = match body.as_ref() {
                Some(body) => request.send_json(body),
                None => request.call(),
            };
            match response {
                Ok(response) => {
                    if response.status() == 204 {
                        return Ok(Value::Null);
                    }
                    return response
                        .into_json()
                        .map_err(|_| anyhow::anyhow!("Discord returned an invalid response"));
                }
                Err(ureq::Error::Status(429, response)) => {
                    let body: Value = response.into_json().unwrap_or(Value::Null);
                    let seconds = body["retry_after"]
                        .as_f64()
                        .filter(|s| s.is_finite() && *s >= 0.)
                        .context("Discord rate limit omitted retry_after")?;
                    if seconds > 60. {
                        bail!("Discord rate limited; retry the durable request later");
                    }
                    std::thread::sleep(Duration::from_secs_f64(seconds));
                }
                Err(ureq::Error::Status(code, _)) => return Err(DiscordHttpError(code).into()),
                Err(_) => bail!("Discord connection failed; request remains pending"),
            }
        }
        bail!("Discord rate limited; request remains pending")
    }
    pub fn preflight(&mut self, config: &DiscordConfig) -> Result<()> {
        preflight_checkouts(config)?;
        let application = self.request("GET", "/oauth2/applications/@me", None)?;
        validate_message_content_flags(application["flags"].as_u64().unwrap_or(0))?;
        let user = self.request("GET", "/users/@me", None)?;
        self.bot_id = field(&user, "id")?;
        let guild = self.request("GET", &format!("/guilds/{}", config.guild_id), None)?;
        let member = self.request(
            "GET",
            &format!("/guilds/{}/members/{}", config.guild_id, self.bot_id),
            None,
        )?;
        let roles = self.request("GET", &format!("/guilds/{}/roles", config.guild_id), None)?;
        for channel in config.channels.keys() {
            let info = self.request("GET", &format!("/channels/{channel}"), None)?;
            if info["type"] != 0 || info["guild_id"].as_str() != Some(&config.guild_id) {
                bail!("configure text channels in the approved Discord server");
            }
            let permissions = channel_permissions(
                &config.guild_id,
                &self.bot_id,
                &guild,
                &member,
                &roles,
                &info,
            );
            let required = 1024 | 2048 | 16384 | 65536 | (1 << 35) | (1 << 38);
            if permissions & required != required {
                bail!(
                    "Discord bot needs View Channel, Send Messages, Embed Links, Read Message History, Create Public Threads, and Send Messages in Threads"
                );
            }
        }
        Ok(())
    }
    pub fn register_command(&self, guild: &str) -> Result<()> {
        snowflake(guild)?;
        let application = self.request("GET", "/oauth2/applications/@me", None)?;
        let app = field(&application, "id")?;
        self.request("POST", &format!("/applications/{app}/guilds/{guild}/commands"), Some(json!({
            "name":"fluent", "description":"Discuss a configured Fluent project in a thread", "type":1,
            "options":[{"name":"project","description":"Configured project label","type":3,"required":true},
                {"name":"message","description":"What should Fluent help with?","type":3,"required":true},
                {"name":"provider","description":"Conversation provider","type":3,"choices":[{"name":"Codex","value":"codex"},{"name":"Claude","value":"claude"}]}]
        })))?;
        Ok(())
    }
    pub fn acknowledge(&self, id: &str, token: &str, accepted: bool) -> Result<()> {
        snowflake(id)?;
        if token.is_empty()
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            bail!("invalid Discord interaction token");
        }
        self.request("POST", &format!("/interactions/{id}/{token}/callback"), Some(json!({"type":4,"data":{
            "content":if accepted { "Received. Fluent will post updates in the project thread." } else { "This action is not authorized in this project." },
            "flags":64,"allowed_mentions":{"parse":[]}
        }})))?;
        Ok(())
    }
    /// Scan bounded pages, failing closed if absence cannot be established.
    fn find_delivery(&self, channel: &str, identity: &str) -> Result<Option<String>> {
        let mut before = String::new();
        for _ in 0..100 {
            let page = self.request(
                "GET",
                &format!("/channels/{channel}/messages?limit=100{before}"),
                None,
            )?;
            let messages = page.as_array().context("Discord history is invalid")?;
            for message in messages {
                if message["author"]["id"].as_str() == Some(&self.bot_id)
                    && message["embeds"].as_array().is_some_and(|embeds| {
                        embeds
                            .iter()
                            .any(|embed| embed["footer"]["text"].as_str() == Some(identity))
                    })
                {
                    return Ok(Some(field(message, "id")?));
                }
            }
            if messages.len() < 100 {
                return Ok(None);
            }
            before = format!("&before={}", field(messages.last().unwrap(), "id")?);
        }
        bail!("Discord delivery history exceeds recovery bound; owner inspection is required")
    }
}
fn field(value: &Value, key: &str) -> Result<String> {
    let id = value[key]
        .as_str()
        .context("Discord response omitted identity")?;
    snowflake(id)?;
    Ok(id.into())
}
fn bits(value: &Value) -> u64 {
    value.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}
fn channel_permissions(
    guild_id: &str,
    bot: &str,
    guild: &Value,
    member: &Value,
    roles: &Value,
    channel: &Value,
) -> u64 {
    if guild["owner_id"].as_str() == Some(bot) {
        return u64::MAX;
    }
    let has_role = |id: &str| {
        id == guild_id
            || member["roles"]
                .as_array()
                .is_some_and(|roles| roles.iter().any(|role| role.as_str() == Some(id)))
    };
    let mut permissions = roles
        .as_array()
        .into_iter()
        .flatten()
        .filter(|role| role["id"].as_str().is_some_and(has_role))
        .fold(0, |bits_, role| bits_ | bits(&role["permissions"]));
    if permissions & 8 != 0 {
        return u64::MAX;
    }
    let overwrites = channel["permission_overwrites"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for group in 0..3 {
        let mut allow = 0;
        let mut deny = 0;
        for overwrite in &overwrites {
            let id = overwrite["id"].as_str().unwrap_or("");
            let applies = match group {
                0 => id == guild_id,
                1 => overwrite["type"] == 0 && id != guild_id && has_role(id),
                _ => overwrite["type"] == 1 && id == bot,
            };
            if applies {
                allow |= bits(&overwrite["allow"]);
                deny |= bits(&overwrite["deny"]);
            }
        }
        permissions = (permissions & !deny) | allow;
    }
    permissions
}
pub fn validate_message_content_flags(flags: u64) -> Result<()> {
    if flags & ((1 << 18) | (1 << 19)) == 0 {
        bail!(
            "Enable Message Content Intent in Discord Developer Portal → Bot → Privileged Gateway Intents; verified apps also need approval. Conversation turns are disabled."
        );
    }
    Ok(())
}
impl DiscordTransport for DiscordRest {
    fn ensure_thread(&mut self, command: &str, channel: &str, project: &str) -> Result<String> {
        let root = Delivery {
            id: format!("command:{command}"),
            channel: channel.into(),
            text: format!("Project: {project}"),
            message_id: None,
        };
        let message = self.deliver(&root)?;
        // Discord uses the root message id as the public thread id.
        match self.request("GET", &format!("/channels/{message}"), None) {
            Ok(existing)
                if existing["type"] == 11 && existing["parent_id"].as_str() == Some(channel) =>
            {
                return Ok(message);
            }
            Ok(_) => bail!("Discord command thread has unexpected authority"),
            Err(error)
                if error
                    .downcast_ref::<DiscordHttpError>()
                    .is_some_and(|e| e.0 == 404) => {}
            Err(error) => return Err(error),
        }
        let created = self.request(
            "POST",
            &format!("/channels/{channel}/messages/{message}/threads"),
            Some(json!({"name":format!("Fluent: {project}"),"auto_archive_duration":1440})),
        )?;
        field(&created, "id")
    }
    fn history(&mut self, guild: &str, thread: &str, after: &str) -> Result<HistoryPage> {
        snowflake(thread)?;
        let after = if after.is_empty() {
            "0"
        } else {
            snowflake(after)?;
            after
        };
        let value = self.request(
            "GET",
            &format!("/channels/{thread}/messages?limit=100&after={after}"),
            None,
        )?;
        let mut messages = value.as_array().context("invalid Discord history")?.clone();
        messages.sort_by_key(|message| {
            message["id"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        });
        let mut page = HistoryPage {
            has_more: messages.len() == 100,
            ..HistoryPage::default()
        };
        for mut message in messages {
            page.last_message = Some(field(&message, "id")?);
            message["guild_id"] = json!(guild);
            message["channel_id"] = json!(thread);
            if let Some(input) = decode_dispatch(&json!({"t":"MESSAGE_CREATE","d":message}))? {
                page.inputs.push(input);
            }
        }
        Ok(page)
    }
    fn corrections(&mut self, guild: &str, thread: &str, inputs: &[Input]) -> Result<Vec<Input>> {
        let mut corrections = Vec::new();
        for original in inputs
            .iter()
            .rev()
            .filter(|input| matches!(input.kind, InputKind::Message { .. }))
            .take(100)
        {
            if inputs
                .iter()
                .any(|i| matches!(&i.kind,InputKind::Delete {message} if message == &original.id))
            {
                continue;
            }
            let response = self.request(
                "GET",
                &format!("/channels/{thread}/messages/{}", original.id),
                None,
            );
            let kind = match response {
                Ok(message) => {
                    let Some(revision) = message["edited_timestamp"].as_str() else {
                        continue;
                    };
                    let text = message["content"]
                        .as_str()
                        .context("Discord Message Content is unavailable")?;
                    InputKind::Edit {
                        message: original.id.clone(),
                        revision: revision.into(),
                        text: text.into(),
                    }
                }
                Err(error)
                    if error
                        .downcast_ref::<DiscordHttpError>()
                        .is_some_and(|e| e.0 == 404) =>
                {
                    InputKind::Delete {
                        message: original.id.clone(),
                    }
                }
                Err(error) => return Err(error),
            };
            corrections.push(Input {
                id: original.id.clone(),
                guild: guild.into(),
                channel: thread.into(),
                actor: original.actor.clone(),
                kind,
            });
        }
        corrections.reverse();
        Ok(corrections)
    }
    fn deliver(&mut self, delivery: &Delivery) -> Result<String> {
        snowflake(&delivery.channel)?;
        let identity = format!(
            "fluent-discord:{:x}",
            Sha256::digest(delivery.id.as_bytes())
        );
        if let Some(id) = self.find_delivery(&delivery.channel, &identity)? {
            return Ok(id);
        }
        let payload = json!({"content":delivery.text,"allowed_mentions":{"parse":[],"replied_user":false},
            "nonce": &identity[15..39], "enforce_nonce":true,"embeds":[{"footer":{"text":identity}}]});
        let result = self.request(
            "POST",
            &format!("/channels/{}/messages", delivery.channel),
            Some(payload),
        )?;
        field(&result, "id")
    }
}
