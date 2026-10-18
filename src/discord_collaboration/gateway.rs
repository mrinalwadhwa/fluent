use super::*;
use serde_json::{Value, json};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GatewaySession {
    pub session_id: Option<String>,
    pub resume_url: Option<String>,
    pub sequence: Option<u64>,
    #[serde(skip)]
    pub heartbeat_ms: u64,
    #[serde(skip)]
    awaiting_ack: bool,
}
impl GatewaySession {
    pub fn handle(&mut self, payload: &Value, token: &str) -> Result<Vec<Value>> {
        if let Some(sequence) = payload["s"].as_u64() {
            self.sequence = Some(sequence);
        }
        match payload["op"].as_u64() {
            Some(10) => {
                self.heartbeat_ms = payload["d"]["heartbeat_interval"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .context("Discord Hello omitted heartbeat interval")?;
                self.awaiting_ack = false;
                Ok(vec![if let Some(session) = &self.session_id {
                    json!({"op":6,"d":{"token":token,"session_id":session,"seq":self.sequence}})
                } else {
                    json!({"op":2,"d":{"token":token,"intents":1 | 512 | 32768,"properties":{"os":"macos","browser":"fluent","device":"fluent"}}})
                }])
            }
            Some(11) => {
                self.awaiting_ack = false;
                Ok(vec![])
            }
            Some(1) => {
                self.awaiting_ack = false;
                Ok(vec![self.heartbeat()?])
            }
            Some(7) => bail!("Discord requested reconnect"),
            Some(9) => {
                if payload["d"] != true {
                    self.session_id = None;
                    self.resume_url = None;
                    self.sequence = None;
                }
                bail!("Discord session requires reconnect")
            }
            Some(0) => {
                if payload["t"] == "READY" {
                    self.session_id = payload["d"]["session_id"].as_str().map(str::to_owned);
                    self.resume_url = payload["d"]["resume_gateway_url"]
                        .as_str()
                        .map(str::to_owned);
                }
                Ok(vec![])
            }
            _ => Ok(vec![]),
        }
    }
    pub fn heartbeat(&mut self) -> Result<Value> {
        if self.awaiting_ack {
            bail!("Discord heartbeat was not acknowledged; reconnect required");
        }
        self.awaiting_ack = true;
        Ok(json!({"op":1,"d":self.sequence}))
    }
}
pub fn gateway_close_failure(code: u16) -> &'static str {
    match code {
        4013 | 4014 => {
            "Discord rejected Message Content access. Enable Message Content Intent in the Developer Portal and obtain approval if required."
        }
        4004 => "Discord rejected the bot credential; replace its Keychain entry.",
        _ => "Discord disconnected; reconnecting with durable history.",
    }
}
pub fn decode_dispatch(payload: &Value) -> Result<Option<Input>> {
    let data = &payload["d"];
    let event = payload["t"].as_str().unwrap_or("");
    if !matches!(
        event,
        "INTERACTION_CREATE" | "MESSAGE_CREATE" | "MESSAGE_UPDATE" | "MESSAGE_DELETE"
    ) {
        return Ok(None);
    }
    if data["guild_id"].is_null() {
        return Ok(None);
    }
    let actor = if event == "INTERACTION_CREATE" {
        &data["member"]["user"]
    } else {
        &data["author"]
    };
    if actor["bot"] == true || !data["webhook_id"].is_null() {
        return Ok(None);
    }
    let kind = if event == "MESSAGE_DELETE" {
        InputKind::Delete {
            message: data["id"]
                .as_str()
                .context("deleted message has no identity")?
                .into(),
        }
    } else if event == "MESSAGE_UPDATE" {
        let (Some(text), Some(revision)) =
            (data["content"].as_str(), data["edited_timestamp"].as_str())
        else {
            return Ok(None);
        };
        InputKind::Edit {
            message: data["id"]
                .as_str()
                .context("edited message has no identity")?
                .into(),
            revision: revision.into(),
            text: text.into(),
        }
    } else if event == "INTERACTION_CREATE" {
        if data["type"] != 2 || data["data"]["name"] != "fluent" {
            return Ok(None);
        }
        let option = |name: &str| {
            data["data"]["options"]
                .as_array()
                .and_then(|options| options.iter().find(|o| o["name"] == name))
                .and_then(|o| o["value"].as_str())
                .map(str::to_owned)
        };
        InputKind::Command {
            project: option("project").context("Fluent command requires project")?,
            provider: option("provider"),
            text: option("message").context("Fluent command requires message")?,
        }
    } else {
        InputKind::Message {
            text: data["content"]
                .as_str()
                .context("Discord message content is unavailable")?
                .into(),
        }
    };
    let get = |v: &Value, name: &str| -> Result<String> {
        let value = v[name].as_str().context("Discord event omitted identity")?;
        snowflake(value)?;
        Ok(value.into())
    };
    Ok(Some(Input {
        id: get(data, "id")?,
        guild: get(data, "guild_id")?,
        channel: get(data, "channel_id")?,
        actor: if actor["id"].is_null()
            && matches!(kind, InputKind::Edit { .. } | InputKind::Delete { .. })
        {
            String::new()
        } else {
            get(actor, "id")?
        },
        kind,
    }))
}
