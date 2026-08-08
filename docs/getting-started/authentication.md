# Authenticate a provider

## Purpose

Connect Mezzanine to a model provider without putting credentials in ordinary
configuration files.

## Prerequisites

- Install `mez` as described in [Install Mezzanine](installation.md).
- Have an account or API credential accepted by the selected provider.

## Sign in interactively

Run the default interactive flow:

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

Noninteractive setup must use an explicit API-key method and an out-of-band
secret source such as an API-key file. For OpenAI, an out-of-band device-code
flow is also available with `mez auth login --device-code`.

## Credential handling

Use `mez auth`, not `config.toml`, for tokens, bearer credentials, and API
keys. Authentication state is stored separately under the user configuration
root and uses an operating-system credential store when available. Normal
status output omits private account identifiers and credential-store references.

Successful authentication does not guarantee a particular entitlement, quota,
or model. Select a model with `/model` or configure a model profile.

## When sign-in fails

Follow the reported action requirement rather than treating an incomplete
browser or device flow as authenticated. Verify the selected provider and retry
its supported credential method. Never paste credentials into an agent prompt
or repository document.

## Related pages

- [First session](first-session.md)
- [Agent and integrations](../agent/README.md)
- [Configuration](../configuration/README.md)
- [Operations and troubleshooting](../operations/README.md)

## Next step

Start [your first session](first-session.md) after authentication succeeds.
