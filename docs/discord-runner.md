# Run Fluent in a private Discord server

The local runner connects outbound to Discord's Gateway under your macOS
account. Configure one owner-approved private server and selected text channels.
Enable **Message Content Intent** under Developer Portal → Bot → Privileged
Gateway Intents. Verified applications also need Discord's approval.

Install the bot with the `bot` and `applications.commands` scopes. In each
configured channel, grant View Channel, Send Messages, Embed Links, Read Message History,
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

Initialize each checkout with Fluent and its provider configuration first. Run
`fluent scheduler run` from the first checkout and keep it running. After
`fluent scheduler status` reports the service as running, run
`fluent scheduler run` from each additional checkout to register it with the
same owner-wide service. Work controls use this existing scheduler.

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


## Authenticate conversation providers

Conversation sessions use private state for each thread. If the first reply
reports that provider authentication is needed, copy the thread channel ID and
run this locally:

```sh
fluent discord provider-login --project app --thread THREAD_ID --provider codex
```

Use `--provider claude` for a thread started with Claude and complete its local
login flow. The provider reads only the bound checkout and writes its thread's
draft directory. The bot token stays in the host transport and Keychain.

## Approve planning and control Work

Ask for Brief, Behaviors, Approach, and Plan in that order. Each complete
planning artifact has a revision-bound approval button. Only a configured
project approver can accept it. Accepting a Brief creates proposed Work;
planning approval does not start execution.

In the bound thread, use:

- `work create` to request an empty proposed Work Item explicitly.
- `work status` to read the linked Work state.
- `work execute` after approving the Plan to request scheduler execution.
- `work resume` to request recovery of the existing latest Attempt.
- `work land` to request landing the latest Merge Candidate.

Consequential requests display a confirmation button. A changed Work state,
a replaced planning revision, or revoked approver makes an old control invalid.
Recovered consequential approvals require a fresh click. Resume and landing
retain Fluent's existing readiness and recovery checks.

The runner stores owner routing, Gateway state, and pending inputs under
`~/.config/fluent/discord/`; project conversations, drafts, and Work bindings
live under `.fluent/work/discord/`. Slack configuration and runtime state stay
separate. Configuration changes take effect on the next input. If the bound
checkout is replaced or its branch or commit changes, update its local
configuration deliberately before resuming conversation access. This includes
updating the pinned commit after an approved landing changes the checkout.

A disconnected runner reconciles provider results and bot delivery markers
before retrying. If provider acceptance cannot be established, it preserves the
input and pauses that turn rather than repeating it. The owner must inspect
that provider session before authorizing any new conversation. Recovery reads
history in bounded pages; if outbound history exceeds the recovery bound, the
runner pauses delivery for owner inspection. After reconnect it also scans accepted ordinary messages in bounded batches
for edits and deletions, retaining a cursor between scans. A deletion retains the original
author and does not claim to know who deleted the message.

## Optional owner-run smoke check

These checks use your private server and real provider accounts. They are not
part of the deterministic test suite and should use a disposable project.

1. Run `fluent discord check`, register the command, and start the runner.
   Confirm `fluent discord status` names safe project labels and the connection.
2. Start a thread with `/fluent`, reply without mentioning the bot, and confirm
   that the selected provider resumes. Repeat with the other provider.
3. Disable Message Content access and restart. Confirm setup fails with the
   intent guidance and no provider turn starts, then restore access.
4. Try a configured participant who is not an approver, an unconfigured user,
   and an unconfigured channel. Confirm none can authorize Work.
5. Approve each planning stage, try an older button after a revision, and remove
   an approver from configuration before another click. Confirm stale and
   revoked controls have no effect.
6. Execute disposable Work, inspect its status, exercise an eligible resume,
   and land only a reviewed ready candidate. Confirm the thread reports each
   resulting state; refresh the checkout's pinned commit after landing.
7. Restart during a reply and a long artifact. Confirm accepted inputs remain,
   visible parts stay ordered, and bot replies do not repeat. Edit and delete
   a processed message and confirm a later attributed correction appears.
8. Include literal `@everyone`, user, and role mention syntax in an artifact.
   Confirm the bot messages produce no notifications. Stop the service when
   the smoke check is complete.
