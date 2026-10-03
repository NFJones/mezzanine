# Authenticate a provider

## Purpose

Connect Mezzanine to a model provider without putting credentials in ordinary
configuration files.

## Prerequisites

- Install `mez` as described in [Install Mezzanine](installation.md).
- Have an account or API credential accepted by the selected provider, unless
  configuring a compatible backend that requires no credentials.

## Sign in interactively

Run the default interactive OpenAI (ChatGPT) flow:

```sh
mez auth login
```

With an interactive terminal, the OpenAI flow prefers browser sign-in. Use the
command's explicit options for a device-code or API-key flow. For an API-key
provider, select it explicitly:

```sh
mez auth login --provider anthropic --api-key
mez auth status
```

Noninteractive API-key setup must use an explicit API-key method and an
out-of-band secret source. To read a key from a private file, pass both
`--api-key` and `--api-key-file`; the file should contain only the key:

```sh
mez auth login --provider anthropic --api-key --api-key-file /secure/path/anthropic-api-key
```

For OpenAI, a device-code flow is also available with
`mez auth login --device-code`.

DeepSeek also supports API-key authentication:

```sh
mez auth login --provider deepseek --api-key
```

For a custom OpenAI-compatible service, configure its API dialect, base URL,
models, and model profiles separately. Store any required key with
`mez auth login --provider <configured-provider-name> --api-key`; custom login
does not create provider connection or model records. A local backend that
requires no credentials does not need `mez auth login`. See
[provider configuration](../configuration/agents-providers-and-auth.md) for
setup, including the LM Studio example in the configuration reference.

## Credential handling

Use `mez auth`, not `config.toml`, for tokens, bearer credentials, and API
keys. Authentication state is stored separately under the user configuration
root. By default, Mez prefers an operating-system credential store and falls
back to a private file there when it is unavailable. Pass
`--credential-store os` to require the operating-system store, or
`--credential-store file` to select the private file store explicitly. An
explicit `os` selection fails if that store is unavailable rather than silently
changing storage backends. Normal status output omits private account
identifiers and credential-store references.

After successful authentication, Mez adds the selected built-in provider's
connection, model-profile, and preset defaults to the primary TOML
configuration. Providers are not added for failed or unattempted sign-ins;
authenticating another provider later adds only that provider without changing
an existing default selection. Explicit YAML and JSON primary configurations
are not rewritten during authentication, so add any required provider and model
entries to those files yourself.

Run `mez config validate` to check configuration separately from
`mez auth status`; valid credentials do not establish that the provider and
model profiles are configured correctly.

Successful authentication does not guarantee a particular entitlement, quota,
or model. In the agent shell, `/model` shows the active profile and configured
profiles; `/model <profile-name>` selects a profile. Use `/model --list` to
inspect the active provider's model catalog, or configure a model profile.

## When sign-in fails

Follow the reported action requirement rather than treating an incomplete
browser or device flow as authenticated. Verify the selected provider and retry
its supported credential method. Never paste credentials into an agent prompt
or repository document.

## Related pages

- [First session](first-session.md)
- [Configure AWS Bedrock through its OpenAI-compatible API](../agent/aws-bedrock-openai-compatible.md)
- [Agent and integrations](../agent/README.md)
- [Configuration](../configuration/README.md)
- [Operations and troubleshooting](../operations/README.md)

## Next step

Start [your first session](first-session.md) after authentication succeeds.
