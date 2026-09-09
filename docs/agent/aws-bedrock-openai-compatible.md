# AWS Bedrock through the OpenAI-compatible API

## Purpose

Configure Amazon Bedrock as a custom Mezzanine provider through Bedrock's
OpenAI-compatible Chat Completions API, without placing an API key in ordinary
configuration.

This is a manual custom-provider workflow. Running `mez auth login --provider
bedrock --api-key` stores credentials, but it does **not** create the Bedrock
provider connection, endpoint, models, or model profiles.

## Prerequisites

- Install Mez and initialize its user configuration.
- Choose one AWS account and Region for both the API key and runtime endpoint.
- Confirm that the desired model supports the
  [Chat Completions API](https://docs.aws.amazon.com/bedrock/latest/userguide/models-api-compatibility.html).
- Complete AWS's
  [prerequisites for model inference](https://docs.aws.amazon.com/bedrock/latest/userguide/inference-prereq.html),
  including the required IAM permissions and any model-specific access steps.

AWS's current [model-access
guide](https://docs.aws.amazon.com/bedrock/latest/userguide/model-access.html)
states that foundation-model access is enabled automatically in commercial
Regions when the account has the required AWS Marketplace permissions. Select
the intended model in the Bedrock model catalog and verify its prerequisites
before production use. Third-party models may start a subscription when first
invoked, and some providers require additional setup; an
`AccessDeniedException` can therefore indicate incomplete model access rather
than a bad Mez configuration.

AWS recommends the `bedrock-runtime` endpoint for new applications. Its
[endpoint comparison](https://docs.aws.amazon.com/bedrock/latest/userguide/endpoints.html)
lists Chat Completions as an OpenAI-compatible API served below
`/openai/v1`. Model and API availability vary by model, endpoint, and Region;
check AWS's current compatibility and endpoint-availability tables rather than
assuming that a Bedrock model supports this workflow.

## Create and store a Bedrock API key

Follow the official [Amazon Bedrock API key
guide](https://docs.aws.amazon.com/bedrock/latest/userguide/api-keys.html):

1. Open the Amazon Bedrock console in the Region you selected.
2. Open **API keys** in the console navigation.
3. For production, generate a short-term key. AWS documents a maximum lifetime
   of 12 hours, or the remaining session duration when that is shorter, and
   recommends short-term keys for production.
4. Use a long-term key only for exploration. AWS documents that this key lasts
   until its configured expiration and creates an IAM user with attached
   policies.

Store the resulting bearer key interactively:

```console
mez auth login --provider bedrock --api-key
mez auth status
```

For noninteractive setup, put only the key in a private file and let Mez read
it out of band:

```console
mez auth login --provider bedrock --api-key \
  --api-key-file /secure/path/bedrock-key
mez auth status
```

Do not put the key in `config.toml`, a command argument, a repository file, or
diagnostic output. Bedrock accepts the key as an `Authorization: Bearer ...`
credential; Mez adds that header from its separate authentication store.

## Configure the provider connection

Find the user configuration with `mez config path`, then add this provider
table. Replace `AWS_REGION` with the same Region used for the API key and model:

```toml
[providers.bedrock]
kind = "openai-compatible"
api = "openai-chat-completions"
auth_profile = "default"
base_url = "https://bedrock-runtime.AWS_REGION.amazonaws.com/openai/v1"

[providers.bedrock.models]
```

For example, a connection in `us-east-1` uses
`https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1`. This is a base
URL, not the full `/chat/completions` request path. Mez derives the Chat
Completions and Models paths from it.

AWS also documents a `bedrock-mantle` endpoint, but the endpoints have
different feature and IAM contracts. This guide deliberately uses the
AWS-recommended `bedrock-runtime` endpoint. Recheck the
[OpenAI-compatible API guide](https://docs.aws.amazon.com/bedrock/latest/userguide/bedrock-mantle.html)
and Mez adapter support before substituting another endpoint or API dialect.

Validate the connection shape before adding models:

```console
mez config validate
```

## Add a model and profile

Bedrock's OpenAI-compatible
[Models API](https://docs.aws.amazon.com/bedrock/latest/userguide/bedrock-mantle.html#bedrock-mantle-models)
exposes `GET /models`. Preview the raw catalog through Mez:

```console
mez config model sync bedrock
```

If the preview contains the intended model and metadata, persist it explicitly:

```console
mez config model sync bedrock --apply
mez config model list bedrock
```

Do not use `--prune` during initial setup. That independent option proposes
removing configured records absent from one live response.

If catalog discovery is unavailable or you need one specific inference target,
copy its exact provider-facing identifier from current AWS documentation or a
Bedrock service response. Pass the complete value as opaque data:

```console
MODEL_ID='copy-the-exact-AWS-model-or-inference-profile-id'
mez config model add bedrock "$MODEL_ID"
```

Model IDs, inference-profile IDs, and ARN-shaped values may contain dots,
slashes, colons, or other punctuation. Do not turn the identifier into a
dotted configuration path. Some Bedrock models require an inference profile
rather than a foundation-model ID; AWS's model card and service response are
authoritative for the value accepted by the selected endpoint.

Add token limits only when AWS documentation or a service response establishes
them. For example, if all three values are known:

```console
mez config model update bedrock "$MODEL_ID" \
  --context-window-tokens CONTEXT_LIMIT \
  --max-input-tokens INPUT_LIMIT \
  --max-output-tokens OUTPUT_LIMIT
```

Omit unknown values rather than guessing them from a model name. Bedrock
remains authoritative and can reject unsupported combinations.

Select the durable provider default and create a named Mez profile:

```console
mez config set providers.bedrock.default_model "$MODEL_ID"
mez config set model_profiles.bedrock-default.provider bedrock
mez config set model_profiles.bedrock-default.model "$MODEL_ID"
mez config validate
mez config layers
```

These commands alter the selected configuration file. Reload configuration or
start a new Mez session before expecting an already-running pane to use the
new provider and profile.

## Validate and select the model

After reload or startup:

```console
mez auth status
mez config validate
```

In the agent pane, inspect and select the catalog:

```text
/refresh-provider-info
/model list
/model bedrock-default
```

`/refresh-provider-info` updates only the running session's best-effort catalog
cache. It does not edit configuration. `mez config model sync bedrock` previews
a durable comparison, and only `--apply` writes model records. A manually added
record remains available even when a later catalog response omits it.

Treat successful configuration and authentication as separate from AWS model
entitlement, quota, and regional availability. A valid profile can still fail
when the AWS account is not authorized to invoke its inference target.

## Rotate or revoke a key

Generate the replacement according to AWS's API-key guide, then overwrite the
stored Mez credential through the same secret-safe flow:

```console
mez auth login --provider bedrock --api-key
mez auth status
```

For file-based automation, replace the private file contents out of band and
repeat `mez auth login --provider bedrock --api-key --api-key-file ...`. Do not
print either key or preserve the old key in shell history.

For compromise response, follow AWS's
[revocation guidance](https://docs.aws.amazon.com/bedrock/latest/userguide/api-keys-revoke.html).
AWS documents deactivate, reset, and delete operations for long-term keys.
Short-term keys cannot be individually deactivated, reset, or deleted; revoke
their usable permissions with IAM policy or end the source session as AWS
directs. Removing a key from Mez does not revoke it in AWS.

## Troubleshooting

- **Authentication failure or expired key:** confirm `mez auth status`, the
  key type and expiration, and the source AWS session. Store a replacement with
  `mez auth login`; never paste it into a prompt or configuration file.
- **Region or endpoint mismatch:** the API key, model availability, and
  `bedrock-runtime.AWS_REGION.amazonaws.com` endpoint must refer to the intended
  Region. Do not use `api.openai.com` or omit `/openai/v1`.
- **Access denied:** verify the IAM identity and the
  [inference prerequisites](https://docs.aws.amazon.com/bedrock/latest/userguide/inference-prereq.html).
  AWS documents `bedrock:InvokeModel` for inference and additional permissions
  for resources such as inference profiles.
- **Model unavailable:** check the current AWS model card, Chat Completions
  compatibility, endpoint availability, account access, and whether the API
  expects a foundation-model ID, inference-profile ID, or ARN.
- **Catalog refresh or sync fails:** AWS documents `/models`, but access,
  endpoint, or service behavior can still prevent discovery. Keep or add the
  exact identifier with `mez config model add bedrock MODEL_ID`; runtime refresh
  is not required for a durable configured model.
- **Tools, structured output, or streaming fail:** OpenAI-compatible does not
  mean every Bedrock model implements every optional OpenAI behavior. Check the
  model's AWS compatibility information and configure only Mez provider options
  that the model and endpoint support.
- **Token-limit rejection:** remove guessed limits, compare configured metadata
  with current AWS model documentation, and lower request or output bounds as
  the provider directs.
- **Wrong API dialect:** this guide requires
  `api = "openai-chat-completions"`. Bedrock's Responses API is a distinct wire
  contract; selecting it requires a Mez Responses-compatible configuration and
  is not interchangeable with this guide.

## Related pages

- [Providers and models](providers-and-models.md)
- [Authenticate a provider](../getting-started/authentication.md)
- [Agents, providers, and authentication](../configuration/agents-providers-and-auth.md)
- [Configuration reference](../configuration/reference.md#providersname)
- [CLI reference](../reference-manual/cli.md)

## Next step

Run `mez config validate`, reload Mez, and use `/model list` to confirm that the
Bedrock profile is selectable before relying on it for production work.
