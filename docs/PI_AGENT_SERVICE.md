# Pi Agent service

`remagic-agentd` is ReMagic's system-owned Pi RPC broker. Applications never receive provider
secrets through the broker, spawn Pi, or select arbitrary command-line arguments. They declare
`agent:pi-v1`; the runner injects these platform-owned values:

- `REMAGIC_AGENT_SOCKET=/run/remagic/agent.sock`
- `REMAGIC_AGENT_TOKEN=<64 lowercase hex characters>`
- `REMAGIC_AGENT_PRINCIPAL=foreground` for the foreground process
- the existing `REMAGIC_APP_ID` and nonzero `REMAGIC_APP_GENERATION`

A managed background service receives a separate token and the `background` principal.
Both principals share the application's persistent Pi worker, profile, and single active-turn
slot. The broker verifies peer UID, exact systemd cgroup component, application ID, principal,
generation, and token. A token is never accepted for another principal or generation.
The current reMarkable application processes still share the device's root UID, so this is a
trusted-application boundary rather than protection against a deliberately malicious root app.

## Wire contract

The Unix stream contains one JSON object per frame, prefixed by a four-byte unsigned
big-endian length. Frames are limited to 1 MiB. Client messages carry top-level
`protocol`, `type`, `request_id`, `app_id`, and `client_token` fields. Protocol 1 supports
`status`, `start_turn`, `cancel_turn`, `tool_result`, `reload_profile`, and `new_session`.
The broker emits `accepted`, `text_delta`, `tool_call`, `complete`, `error`, and `status`.

`start_turn.lane` is one of `interactive`, `speculative`, or `scheduled`. An interactive
turn cancels and replaces speculative or scheduled work for the same application. Neither
background lane can replace an interactive turn. A client disconnect cancels the turn that
was accepted on that connection.

## Pi runtime and credentials

Formal releases require `REMAGIC_PI_RUNTIME_DIR` while running
`scripts/build-system-release.sh`. The self-contained directory must contain executable
`bin/node` and `bin/pi`, plus a `runtime.env` that records
`REMAGIC_PI_RUNTIME_SCHEMA`, `REMAGIC_PI_VERSION`, and `REMAGIC_NODE_VERSION`. These versions
are copied into release metadata and verified again during device installation. Build-time
links are dereferenced so every installed runtime file is covered by the release checksums. Runtime
selection never falls back to Paperweight/AppLoad's historical `/home/root/node/bin/pi`; a missing
packaged runtime is reported as unavailable. Developers may opt into another binary only with the
explicit `REMAGIC_PI_BINARY` service override, which is exposed as `runtime_source=override`.

`scripts/build-pi-runtime.sh` produces the pinned ARM64 runtime (Pi 0.81.1 and Node 22.23.1
by default) from the official Node checksum list and the exact npm package version. Pass its
output directory as `REMAGIC_PI_RUNTIME_DIR`; version overrides remain explicit build inputs and
are recorded in `runtime.env`.

Provider keys live outside applications and release payloads:

```text
/home/root/.config/remagic/secrets/providers/deepseek.env
/home/root/.config/remagic/secrets/providers/openai.env
```

Each file must be a regular, non-symlink file owned by the service UID with mode `0600`.
Only `DEEPSEEK_API_KEY` or `OPENAI_API_KEY`, respectively, is imported into Pi's cleared
environment. The service never logs key values.

Run `./configure-provider.sh deepseek` or `./configure-provider.sh openai` on the connected
computer to update a key without placing it in process arguments or terminal output. An optional
`BASE_URL=https://...` in the same private file creates an application-isolated `models.json`;
that file references `$DEEPSEEK_API_KEY` or `$OPENAI_API_KEY` and never contains the secret.

Pi runs persistently per application in RPC mode with session persistence disabled. Skills,
prompt templates, context files, and approval prompts are disabled. With `profile.tools=false`,
all tools and extension discovery are disabled. With `profile.tools=true`, Pi still disables
every built-in tool and loads only ReMagic's fixed `remagic-tools.js`; its bounded `web_search`
can contact only the hard-coded search endpoint and has no shell or filesystem access. Client
supplied tool definitions fail closed.

The broker process is bound to `remagicd.service`. Restarting the manager therefore discards
all in-memory generation/token bindings and Pi children before new managed application
generations are issued.
