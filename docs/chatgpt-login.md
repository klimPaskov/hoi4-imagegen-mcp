# ChatGPT login

[Documentation index](../README.md#documentation)

The ChatGPT-login provider makes images on your own ChatGPT plan, with no API key. It is a first-class local route:
- It ships in every local build, with no feature flag, config entry or environment variable.
- You sign in once in the browser. The risk note is shown as information, and signing in accepts it. There is nothing to type.
- Once you are signed in, it is the default local provider. An explicit `provider` in a plan still wins.
- Documented API providers stay available: OpenAI API key, Gemini, fal and ComfyUI.
- It is local only and is never part of a hosted deployment (see "Hosted deployments" below).

## Sign in

```
hoi4-imagegen login
```

An agent or a setup tool can start the same sign-in with the MCP tool `hoi4.image_login`, with no terminal at all. It opens the browser and returns at once, and the sign-in finishes in the background.

1. **The risk note.** It is printed in the terminal (or returned by `hoi4.image_login`), and the page the browser lands on after the sign-in sums it up. Signing in is the acceptance, so there is no question to answer. The acceptance is recorded once per user, in your own data folder, never in a project. `--accept-risk` is still accepted and does nothing.
2. **The browser.** It opens OpenAI's sign-in page, and the address is also printed. This is an OAuth 2.0 authorization-code flow with PKCE against `auth.openai.com`, using the public client id of the official Codex CLI and a loopback redirect on `127.0.0.1:1455`. Your password is typed only into OpenAI's page.
3. **The tokens.** They go to a store only your user account can read:
   - Windows: Credential Manager, service `hoi4-imagegen.chatgpt-login`, split across numbered entries because of Windows' size limit
   - macOS: a private file (mode 0600, in a 0700 folder), `~/Library/Application Support/hoi4-imagegen/chatgpt-login/auth.json`, since 0.4.0
   - Linux: the Secret Service

   Linux without a running keyring falls back to a private file (mode 0600) in `~/.local/share/hoi4-imagegen/chatgpt-login/`. A version 0.1 credential file is moved into the store on first use and deleted.

   **Why not the macOS Keychain.** A Keychain item trusts only the program that created it. The sign-in is made by `hoi4-imagegen` and read by `hoi4-imagegen-mcp`, so macOS asked for the Keychain password on every start of the MCP server and again after every release. An agent cannot answer that dialog. The private file is as safe as the Codex CLI's own sign-in file. To keep the Keychain anyway, set `HOI4_IMAGEGEN_CHATGPT_STORE=keychain` for both programs and answer the dialog. After updating from 0.3.x on a Mac, run `hoi4-imagegen login` once: the old Keychain sign-in is not read, and `login` or `logout` removes it.

Other commands:
- `hoi4-imagegen login-status` shows the account and plan.
- `hoi4-imagegen logout` removes the tokens.
- The MCP tool `hoi4.image_status` reports `chatgpt_login.signed_in`.

## The model is fixed

The sign-in route uses ChatGPT's current image model, which is the latest one, and the backend picks it. Model choice such as GPT-Image 2.6 applies to the OpenAI API route, not to this one. The outer model and effort below are what the route fixes and checks.

Every image runs on **gpt-6-luna**. There is no other model and no model fallback.

The reasoning effort is **the highest one the backend accepts**. The backend can refuse `high` for image requests ("Complexity 'high' is not supported for this image model"), so the route steps down:
- Each request asks for `reasoning.effort = high` first. If the backend refuses that effort, the same request is sent once at `medium`, then once at `low`, and finally once with no `reasoning` field at all, so the backend uses its own default effort. The first rung the backend accepts is used.
- The backend passes the effort to the image tool as its `complexity`, and it has refused high, medium and low at times. An image made on the last rung records `reasoning_effort: "backend default"` and the effort the backend reported.
- A refused effort is remembered on the machine for a day (`refused_efforts` in `route-state.json`), so later images skip it and send one request. The last effort on the ladder is always tried, so a backend that changes again is noticed. After the day, `high` is tried again.
- The attempts of one image run one after another in that image's slot and keep the slot's pacing (see "Requests at once" below).
- If the backend refuses all four, the call ends with one clear error that lists each refusal, and the route is down until the backend changes. Nothing is retried after that.
- Any other backend error ends the call at once. It is not retried at a lower effort.
- The backend's response events must report the same model (or a dated snapshot of it, `gpt-6-luna-*`) and the effort that attempt asked for. Anything else, or a response that does not say, is refused with a clear error, and the image is never used.
- The effort each image ran at is data on its candidate record in the ledger: `usage.reasoning_effort` (a named effort or `backend default`), `usage.reported_effort` (what the backend said it ran at), `usage.refused_efforts` and the backend's own `usage.backend_usage`. The candidate notes say the same in words ("ran on gpt-6-luna at medium reasoning effort").
- A plan that names another model for this provider is refused at plan time.
- The model and effort the backend reported are recorded with every candidate in the ledger.

## The image model

The sign-in route asks for an image the way ChatGPT does: the request's `image_generation` tool carries no `model`, and ChatGPT draws the picture with its current image model. The route cannot choose one per family. Naming `gpt-image-2.6-sunburst` or `gpt-image-2.6-flare` on the tool was accepted and ignored by the backend, and the response listed the same image model both times (recorded in `crates/provider-chatgpt-login/tests/fixtures/recorded-real/tool-model-*-ignored-2026-11-08.sse`). So the route keeps the backend's choice and treats it as ChatGPT's latest image model. The model each image ran on is recorded in its candidate note.

## Requests at once

The route paces itself across every process on the machine, because a sign-in can be revoked when too many requests arrive at once. Up to ten requests run at the same time by default, and each slot waits between its own requests. If the server signs you out, the route does not wait for hours: after you sign in again it carries on, and every sign-out lowers the number in use by one.

- **Slots.** Every request the route makes takes one slot: a generation, a token refresh and the sign-in code exchange. A slot is an operating system lock on a file in the per-user data folder, next to `route-state.json`. Slot 0 is `route.lock`, the same file older versions lock, and slot N is `route.lock.N`, created the first time a caller reaches it (there is no fixed number of slots). Versions up to 0.4.6 use only the first six, so they and a newer version never send at the same time through a shared slot, and together they never exceed the largest configured number. A caller takes the first free slot. When all are busy it waits, and after 10 minutes it gives up with an error that says every slot is in use. Every process on the machine shares the same slots, and a slot is freed when its process ends, even after a crash.
- **How many.** Ten by default. Set `HOI4_IMAGEGEN_CHATGPT_CONCURRENCY` to change it: 1 gives the old one-at-a-time behaviour, and there is no upper limit. A project can also set it in `.hoi4-imagegen/config.toml`, and the environment variable wins:

  ```toml
  [providers.chatgpt_login]
  concurrency = 4
  ```

  A number above 10 takes effect only after you confirm it, because the more requests arrive at once the more likely OpenAI is to revoke the sign-in (it did on 2026-11-02 with several tools at once). Confirm it beside the number, or with the matching environment variable:

  ```toml
  [providers.chatgpt_login]
  concurrency = 16
  concurrency_confirmed = true
  ```

  ```text
  HOI4_IMAGEGEN_CHATGPT_CONCURRENCY=16
  HOI4_IMAGEGEN_CHATGPT_CONCURRENCY_CONFIRMED=1
  ```

  Without the confirmation the route uses 10, and `hoi4.image_status` (`chatgpt_login.route.concurrency.reason`) and `doctor` say so. Each process uses its own setting, so keep it the same for every tool on the machine.
- **Pacing.** Each slot waits at least 20 seconds after its own last request. While the route runs one at a time, the gap counts from the last request of any slot, as before.
- **Backoff.** A 429 starts a backoff of 30 seconds that doubles with every further 429, up to 5 minutes. It holds every slot.
- **Fall back to one at a time.** A 429 or the backend's usage-limit message drops the route to one request at a time on the whole machine, for six hours after the last one (every new one restarts the six hours). The reason and the time are recorded in `route-state.json` (`fallback`), so every process sees it. A caller that already holds a higher slot gives it up before its next request and waits for slot 0. A sign-out by the server no longer does this (before 0.4.7 it did).
- **Sign-outs.** When the server signs you out (a 401, or an answer that says `token_revoked`) the route records it (`revoked` in `route-state.json`), makes no more requests, and says so everywhere: sign in again with `hoi4-imagegen login`. After that it carries on. Every sign-out lowers the number of requests at once by one, down to one: from 10 to 9 at the first, then 8, and so on. That lower number is the learned limit. It is kept in `route-state.json` (`step_down`), so every process on the machine uses it, it survives a restart, and a new sign-in does not put it back. A configured number above the learned limit does not raise it. `hoi4.image_status` (`chatgpt_login.route.concurrency`: `effective`, `learned_limit`, `sign_outs_seen` and `reason`), `doctor` and `login-status` show the number in use and why. Raising it again is your choice: clear the learned limit, and the configured number applies (a number above 10 still needs the confirmation above):

  ```powershell
  hoi4-imagegen reset-concurrency
  ```
- **"Unable to verify model access".** The backend sometimes answers "Unable to verify model access right now" on a few requests while others succeed. It comes as an error event in the stream or in an HTTP error body, and the match ignores case. It is a temporary condition, not a terminal bad request:
  - The first answer fails that request with a retryable error. Its slot waits 60 seconds before its next request (`slot_pause` in `route-state.json`). The other slots keep running.
  - A second answer within 15 minutes of the last one, from any slot or process, drops the route to one request at a time with the reason `model_access_unverified`, through the same mechanism and warning as above. It lasts one hour after that answer, not six, because the message looked like a passing hiccup and not a sign-out or a plan limit. Every further answer within 15 minutes of the last restarts the hour. It never cuts a longer fall back short.
  - The count and the time of the last answer are recorded in `route-state.json` (`model_access`) and shown by `doctor`, `login-status` and `hoi4.image_status` (`chatgpt_login.route.model_access_unverified`, with the slots still paused).
- **The warning.** While it holds, every generation from this route says so, in the image's notes or in the error. `hoi4-imagegen doctor`, `hoi4-imagegen login-status` and `hoi4.image_status` (`chatgpt_login.route.fallback` and `chatgpt_login.route.concurrency`) show it too. It reads like this:

  ```
  warning: the ChatGPT-login route dropped to one request at a time on this machine after a rate limit (http_429) at 2026-01-05T10:00:00Z; it goes back to several at a time after 2026-01-05T16:00:00Z if no new sign-out or rate limit arrives
  ```

  After two "Unable to verify model access" answers it reads:

  ```
  warning: the ChatGPT-login route dropped to one request at a time on this machine after the backend failing twice within 15 minutes to verify model access (model_access_unverified) at 2026-01-05T19:46:00Z; it goes back to several at a time after 2026-01-05T20:46:00Z if no new model-access error arrives
  ```
- **The state file.** Several slot holders write `route-state.json` at once, so every read and every change of it happens under a short lock on `route-state.lock`. The file keeps the last request of each slot (`slot_last_request`). A file written by 0.3.2 or older still reads: its single `last_request_epoch` paces every slot until the slots have their own times.

## What to know before using it

- It presents itself as the Codex CLI to a backend that OpenAI does not document for other tools. It can change or be blocked without notice.
- Heavy use counts against your plan's limits. In the worst case OpenAI could restrict the account. The consequences are yours.
- Whether this use fits OpenAI's terms has not been confirmed; see [rules and terms](rules-and-terms.md). OpenAI's guidance is to use Platform API keys for general API calls.
- Spend is recorded as unknown, since there is no per-image price or usage report. It never charges an operator allowance.

## What changed from upstream (Connected-Mate/gptimage `dfb4948`)

| Upstream | Here |
|---|---|
| Read `~/.codex/auth.json` as a fallback and wrote rotated tokens back into it | Never touches the Codex CLI's credentials, so it cannot rotate that tool's refresh token behind its back |
| Plain JSON token file | Operating system credential store |
| `GPTIMAGE_ACCESS_TOKEN` and `GPTIMAGE_MODEL` environment variables | Removed; the model is fixed |
| No check of what the backend ran | The model and effort must be echoed, or the image is refused |
| Identifiers scattered through the code | One block of constants at the top of `crates/provider-chatgpt-login/src/lib.rs` |

## Hosted deployments

`RunMode::Hosted` never registers the provider. `imagegen_mcp::plugin::refuses_hosted(RunMode::Hosted)` is `true`, and `crates/imagegen-mcp/tests/plugin_gates.rs` checks that a hosted engine cannot select it, even signed in and named in the project file.

## Recorded responses

Set `HOI4_IMAGEGEN_RECORD_RESPONSES=<folder>` to save each raw backend response. The fixtures under `crates/provider-chatgpt-login/tests/fixtures/recorded-real/` were made this way. They are reviewed before commit and labelled "recorded-real replay", and faults are injected into them for CI.

## What the backend controls

The first signed-in probe ran on Windows. Its fixture is `crates/provider-chatgpt-login/tests/fixtures/recorded-real/focus-luna-high-2026-11-01.sse`, and the evidence is `docs/evidence/2026-11-01-luna-probe.json`.
- The backend ran the request on `gpt-6-luna` with `reasoning.effort = high`, as both `response.created` and `response.completed` report.
- The pixels come from the backend's hosted image tool, which is ChatGPT's current image model, the latest one.
- The backend replaces the tool settings we send with `quality = auto` and `size = auto`. The probe image came back at quality `medium`, 1337x1176, with a transparent background.

So the route forces and verifies the outer model and effort, but it cannot choose the image tool's model, quality or size. The ledger records what the backend reported for every image.

**What the backend chooses, measured.** The gallery runs recorded the image tool's report for 178 images. Every report named `gpt-image-2-codex` as the image model. The request asked for quality `high` each time. The backend chose by background, not by what was asked:

| Background | Images | Quality the backend chose |
|---|---|---|
| Transparent (icons, emblems, counters) | 92 | `medium` every time |
| Opaque (portraits, events, technology, operation and decision pictures) | 86 | `low` 61, `medium` 24, `high` 1 |

One more probe also asked for quality `high` in the outer model's instructions. A leader portrait still came back at `low`, 1077x1460. The sign-in route therefore has no way to raise quality, and the model routing (Flare for icons, Sunburst for detail) applies only to the OpenAI API route. The counts are in `docs/evidence/2026-11-13-signin-quality.json`. For a detailed picture that must look its best, the OpenAI API route with `gpt-image-2.6-sunburst` at quality `high` is the choice. It is a paid route, so it asks before spending.
