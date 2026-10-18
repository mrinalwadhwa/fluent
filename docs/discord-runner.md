# Run Fluent in a private Discord server

The local runner connects outbound to Discord's Gateway under your macOS
account. Configure one owner-approved private server and selected text channels.
Enable **Message Content Intent** under Developer Portal → Bot → Privileged
Gateway Intents. Verified applications also need Discord's approval.

Install the bot with the `bot` and `applications.commands` scopes. In each
configured channel, grant View Channel, Send Messages, Read Message History,
Create Public Threads, and Send Messages in Threads. Keep the server private.

In Keychain Access, create a login Keychain password with service
`dev.fluent.discord` and account `bot-token`. Store the bot token as its password.
Do not put the token in configuration files, environment variables, or commands.

Create `~/.config/fluent/discord/config.json`:

```json
{
  "guild_id": "111111111111111111",
  "channels": {"222222222222222222": ["app"]},
  "projects": {
    "app": {
      "checkout": {
        "label": "main",
        "path": "/absolute/path/to/app",
        "identity": "checkout-v1:REPLACE",
        "branch": "main",
        "commit": "REPLACE_WITH_HEAD"
      },
      "participants": ["333333333333333333"],
      "approvers": ["333333333333333333"],
      "provider": "codex"
    }
  }
}
```

Use Discord Developer Mode to copy server, channel, and user IDs. Run
`fluent discord checkout-identity /absolute/path/to/app` for the checkout
identity and `git rev-parse HEAD` in that checkout for its commit. Labels must
contain only letters, digits, hyphens, or underscores. Add more projects and
channel mappings as needed. Providers may be `codex` or `claude`.

Run `fluent discord check`, then `fluent discord register`. Start with
`fluent discord run` in the foreground, or use `fluent discord enable` to install
the login service. `fluent discord status`, `restart`, and `stop` inspect and
control the service. Provider executables must be on the service PATH.

Invoke `/fluent project:app message:plan this` in a configured text channel.
Fluent creates a public thread; ordinary replies there continue the selected
provider session. Only configured participants can access the checkout.
Discord channel permissions do not grant project approval authority.

Protocol references: [Gateway](https://docs.discord.com/developers/events/gateway),
[interactions](https://docs.discord.com/developers/interactions/receiving-and-responding),
[threads](https://docs.discord.com/developers/resources/channel#start-thread-from-message),
[messages](https://docs.discord.com/developers/resources/message), and
[rate limits](https://docs.discord.com/developers/topics/rate-limits).
