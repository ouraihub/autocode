# Build and run autocode locally

autocode is an independent project. No public download or release endpoint is designated here. Do not use historical upstream installers or update feeds as autocode packages.

## Windows x64 portable package

Run these commands from the repository root in Command Prompt:

```bat
scripts\msvc\setup-msvc.bat
scripts\msvc\package-exe.bat
```

The first command prepares the portable MSVC toolchain. The packaging wrapper loads that environment, prepares Spectre-mitigated libraries, and builds the `autocode`, `cli`, and `auto_update_helper` Cargo packages using the locked dependencies. The default profile is `release-fast`; select the release profile with:

```bat
scripts\msvc\package-exe.bat -Profile release
```

The package includes the application, CLI, update helper, and terminal runtime files:

- Application: **`dist/autocode/autocode.exe`**
- Portable directory: `dist/autocode/`
- Portable archive: `dist/autocode-x64.zip`

Keep the directory contents together. The EXE is the application itself, not an installer. To move it to another machine, copy the complete portable directory or unpack the archive there.

For an isolated local run without reusing normal application data:

```bat
dist\autocode\autocode.exe --user-data-dir "%TEMP%\autocode-local"
```

For a development build rather than packaging:

```bat
scripts\msvc\cargo-msvc.bat build -p autocode --bin autocode
```

See the [MSVC toolchain guide](../scripts/msvc/README.md) for setup prerequisites and environment overrides. These commands document build targets; they are not a claim that a build or runtime verification has passed.

## Application identity and logo

The Cargo package, executable and source module are named `autocode`; the main
source tree is `crates/autocode`. Project configuration uses `.autocode`, and
the default user configuration/data belongs to autocode rather than the other
product. Existing data from the other version is not moved or deleted.

The primary vector logo is `assets/images/autocode_logo.svg`. Its code brackets
enclose a geometric A; the matching monochrome toolbar marks are
`assets/icons/autocode.svg` and `assets/icons/autocode_mark.svg`. Desktop PNG,
ICO and ICNS resources use the same geometry. There is no website in this tree.

Automatic updates have no default remote source. Use the local package script,
or explicitly configure `AUTOCODE_RELEASE_REPOSITORY=owner/repository` for your
own release assets; this never selects another product's update feed.

Verification of this migration: the `autocode` main binary passed
`scripts\msvc\cargo-msvc.bat check --locked -p autocode --bin autocode`.
`scripts\msvc\package-exe.bat` then completed successfully with exit code 0.
The archive contains the editor, CLI, update helper and both terminal runtime
files. The packaged CLI resolved `autocode.exe`, the editor's `--help` exited
successfully, and an isolated native GUI launch displayed the onboarding window
with the autocode name and new logo. The SVG was also rendered separately.

The auto-update download regression initially timed out: its lock path depended
on a real user-data directory, and its wait loops did not await native disk I/O.
Tests now use OS-temporary lock paths and owned temporary installer directories,
skip installed-application cleanup, drive both GPUI executors, and await the
actual download task before asserting installation and restart results.
The regression also initializes a per-App in-memory database, so the installed
version record never touches the developer's database. Final command:
`scripts\msvc\cargo-msvc.bat test --locked -p auto_update --lib test_auto_update_downloads -- --test-threads=1`.
Result: **1 passed, 0 failed, 7 filtered out**, exit code 0; checks release lookup,
download, installation override, version state and restart path using a fake HTTP
client. No real remote release or installer was executed.

## Model services

Open Settings (`Ctrl+,`) and select **Model Services / 模型服务** in the left
navigation. This is a real settings page, not a settings.json section. The chat
panel's server button opens the same management view.

Opening Model Services seeds the built-in 七牛云, 硅基流动 and Fenno templates,
matching auto-craft's public catalog. They start disabled with no models or keys.
Each has a stable preset identity: renaming or editing never causes duplicates or
overwrites on the next open. Existing custom services with a matching name are
preserved instead of replaced. Built-ins may be edited or disabled, but not deleted;
their key section links to the corresponding public key-registration page.

1. Choose **New Provider** to open the name, protocol, API endpoint and access-mode
   dialog. Use the selected provider's settings icon or row menu to rename it.
2. Enter the first API key and press Enter or leave the field. Saved keys appear
   in a compact masked row; the key-management button opens a separate dialog for
   labels, replacement, copying, enabling and removal. Credentials and request-header
   values stay in the system vault, not generated configuration or JSON.
3. Edit the API address inline (Enter/blur saves). The adjacent settings icon opens
   the protocol, URL, secret headers and default-line editor. **Add endpoint** adds
   another protocol; the last endpoint cannot be removed.
4. **Check connection** and **Sync models** first save a pending first API key,
   wait for vault read-back and metadata commit, then use the returned provider
   snapshot. A click during the blur-triggered save continues after it completes.
   Credential failures retain the input and stop before any HTTP request. Detection
   checks the saved endpoint's model-list API and authentication, not generation.
   **Sync models** opens a discovery picker: add/remove only the models you choose.
   Models not returned by discovery remain unchanged.
5. The model toolbar provides search, capability filtering, group folding, sync and
   manual addition. The model settings dialog edits type, protocols, token limits,
   capabilities and declared thinking levels. A model row's menu sets Pi/OMP/DSH
   defaults for new chats; non-chat models do not appear in the chat picker.
6. Direct mode rotates enabled keys per engine launch. Gateway mode creates an
   authenticated `127.0.0.1` lease and rotates keys per upstream HTTP request.
   Responses stream through; disconnects are not retried. Closing the run releases
   its gateway and generated support directory. No-authentication services also
   use the gateway so native engine credentials cannot leak to the endpoint.
   DSH custom headers use the gateway because its provider config has no safe
   environment-reference header format.

The existing Claude Code/Codex/OpenCode native configuration remains independent.
Service/model changes affect a newly prepared run; they do not rewrite an already
running child process. An unavailable provider/model or credential reports an error
instead of silently switching to a different service.

The model-service surface follows auto-craft's provider page and its locked
LumaKit presentation (`ce23b868`): 208px provider rail, 36px provider rows,
16px detail-section spacing, grouped 32px model rows and separate editor dialogs.
Narrow content stacks the rail above the detail. The existing autocode settings
navigation and theme remain the host; no second GPUI runtime is embedded.
Model type and supported thinking levels persist in existing model-definition
JSON. Older definitions deserialize as chat models with unspecified thinking levels.

Provider rows have independent visible `…` menus. Editing or cancelling deletion
of another provider does not change the current selection. Delete confirmation
names its target; built-in templates keep deletion disabled.

Inline changes save sequentially: edits made during a write are retained and
saved afterward. A failed save keeps the draft and exposes **Retry**, including
when the endpoint text is unchanged. The endpoint preview uses the same URL
builder as discovery, and multi-line selectors display both protocol and URL.

Connection checks report each enabled key separately and allow disabling a key
from its result row. Compact saved-key rows show labels and extra-key counts;
copy/edit controls appear on hover or keyboard focus. Discovery **Add all** /
**Remove all** acts only on the current search/type results, preserving unlisted
models and other protocol memberships. Search expands matching groups and
updates type counts. Group headers are full-width controls, and model settings
mark the default endpoint and explain next-run activation.

Focused regression commands:

```bat
scripts\msvc\cargo-msvc.bat test --locked -p agent_chat --lib model_services -- --test-threads=1
scripts\msvc\cargo-msvc.bat test --locked -p agent_chat --lib provider_services::probe::tests -- --test-threads=1
```

The interaction regressions passed **8/8** and the provider probe regressions
passed **2/2**. These tests use isolated application databases or test repositories;
the probe tests use a controlled HTTP client rather than real provider credentials.

The updated Windows portable package built successfully with
`scripts\msvc\package-exe.bat`. An isolated launch of that packaged EXE verified
visible provider menus, disabled built-in deletion, named delete confirmation,
cancel/confirm behavior, per-key failed/successful detection against a local HTTP
endpoint, disabling the failed key, search expansion and filtered bulk add/remove
without removing the embedding model outside the search. An 850px-wide settings
window retained the stacked provider list and visible menus. The temporary
provider and its test credentials were removed through the application afterward.
Keyboard focus/hover appearance was not conclusively verified in the native
session because the terminal interrupted focus; save-failure retry is covered by
the regression rather than a completed native failure scenario.

## Chat agent and model controls

The chat sidebar always shows a **+** menu, including after the first chat and
in narrow sidebars. Choosing another agent creates a separate chat and preserves
the original conversation; it does not change the engine attached to old history.

Pi/OMP/DSH model pickers include enabled chat models from Model Services. Saved
services are loaded at startup, and enabling/disabling a service refreshes the
catalog. A finished empty native catalog is not an indefinite loading state.
The composer identifies the agent, service and model ID; when a native engine
has not reported its default model, the label says so rather than naming only
the agent. Runtime model reports do not overwrite a managed model selection.
While a request runs, the composer status shows startup/waiting/input state.

OMP RPC v2 text is read from `assistantMessageEvent.delta`; text-end snapshots
are not appended twice. Native model selection is verified before prompting,
and prompt errors/aborts are not treated as successful replies. Native catalog,
reasoning, session resume and RPC UI answer paths use correlated request IDs.

Verification: sidebar rendered-control regression, managed catalog cold-start /
selection / route regression and runtime-model identity regression passed.
OMP integration tests passed **7/7**, reasoning integration **1/1**, and OMP
parser tests **3/3**. A real installed `omp.exe` also negotiated RPC v2, selected
an isolated registered model, requested `/v1/chat/completions` from a local HTTP
endpoint and emitted `LOCAL_RESPONSE_OK` as a `text_delta` before a completed
`prompt_result`. This did not call a real model provider or use user credentials.

The release-fast build completed successfully. Because the previous portable EXE
was running, the updated runtime was collected separately as
`dist/autocode-chat-fix/autocode.exe` and `dist/autocode-chat-fix-x64.zip` without
overwriting or closing the user's instance. Native verification with this new EXE
and an isolated data directory showed the previously saved LocalSmoke service
model in chat search. Selecting it displayed `OMP · LocalSmoke · smoke-chat`;
two sends returned `LOCAL_RESPONSE_OK` through a real OMP child and local HTTP
endpoint. A delayed response visibly showed the running/waiting status and Stop
control. The persistent sidebar **+** menu then opened a new Codex chat while the
OMP conversation and its responses remained in the list and tab. The isolated
instance and endpoint were closed afterward; no real provider request was sent.

Pi uses the same `assistantMessageEvent.delta` field for text/thinking, while
`text_end`/`thinking_end` contain final snapshots rather than additional text.
Its adapter now maps those deltas, reports tool results against their call IDs,
reads tool arguments from `args`, and retains the state response's session file
when `agent_settled` contains only `aborted`. Assistant errors and aborted turns
are not reported as completed success; runtime state exposes the model identity.
Pi integration **3/3**, shared reasoning integration **1/1**, and Pi parser
tests **2/2** passed. The installed Pi Node CLI also requested a controlled local
`/v1/chat/completions` endpoint and emitted `PI_LOCAL_RESPONSE_OK` as `text_delta`
followed by a separate snapshot and `agent_settled { aborted: false }`.
The Pi repair release-fast build completed successfully and was collected as
`dist/autocode-pi-fix/autocode.exe` and `dist/autocode-pi-fix-x64.zip`, leaving the
running user instance untouched. An isolated launch of this EXE selected Pi and
the configured local model, ran the real Pi CLI against a controlled HTTP
tool-call response, executed `echo PI_TOOL_OK`, and rendered
`PI_LOCAL_RESPONSE_OK`. The saved transcript contains a completed tool with
`PI_TOOL_OK\n` output followed by exactly one assistant text entry. The isolated
app and local endpoint were closed after verification; no user credentials or
real model endpoints were used.

Agent icons share one `AgentKind::icon` mapping across history, chat tabs,
model controls and both new-chat menus. Claude/Codex/OpenCode retain their marks;
Pi uses a pi symbol, OMP a hexagonal monogram, and DSH a terminal prompt. Each
new-chat menu item includes its matching icon and text, so identification does
not depend on color. SVG fills follow the host theme's foreground color.
The icon update built successfully and is packaged separately as
`dist/autocode-agent-icons/autocode.exe` and `dist/autocode-agent-icons-x64.zip`.
An isolated native launch showed distinct Pi/OMP history and tab icons and all
six icons in the new-chat menu. Selecting OMP created a chat with the same icon
in history, tab and composer. The rendered-menu regression passed; registered
icon files passed their existence check. The separate dangling-icon check failed
on the existing unregistered `autocode_mark.svg`, not the newly added icons.

The chat toolbar's Model Services shortcut uses an opaque, bordered modal
surface with a title and explicit Close button, instead of placing the transparent
settings view directly over chat. The host modal layer dims/occludes the background;
the surface height accounts for its top offset and keeps the close control inside
the viewport. Escape uses the existing menu cancel action, while nested service
editors retain their own Escape handling. The focused modal regression passed:
open over a chat, check bounds, click Close, preserve the active chat, and reopen /
toggle closed. The standalone Settings page keeps its existing layout.




## macOS and Linux

Build from this checkout using the repository's `script/bundle-mac` or `script/bundle-linux` scripts and platform prerequisites. The [Zed development documentation](https://zed.dev/docs/development) provides background for the inherited toolchain. This project does not advertise prebuilt macOS or Linux downloads here, and historical upstream release artifacts are not releases of autocode.
