# compound-562, the untested hooks measured (wsp-298)

Addendum to `docs/spikes/compound-562.md` in the compound repo (the spike itself; this file is its "Left undone",
row 1 of the `ask-in-compound` build). Measured 2026-10-19 on **Claude Code 2.1.303** (the spike used 2.1.302),
`haiku`, a scratch hook in scratch directories under the session scratchpad, `--setting-sources project`
(`project,local` where a row says so). Ed's settings were not read or written; the one place the bypass-permissions
warning dialog had to be skipped it was done with a scratch `--settings` file, not by accepting it. No `src` changed.
The hook, the pty driver and every payload log are in the session scratchpad, not the repo.

The hook is the spike's: log every payload, then reply from a policy file keyed by `event/tool`. A trial is one real
`claude` in a pty, driven by keystrokes, with the reply scripted.

## Answers

| # | question | answer |
|---|---|---|
| 1 | `PermissionRequest` allow on `ExitPlanMode` with `updatedPermissions` `setMode`: does it reproduce the pane's auto-accept option? | **Yes, but not as the spike wrote it.** A bare `allow` does **not** approve the plan: the hook output is parsed and accepted ("Successfully parsed and validated hook JSON output") and the dialog stays up, 60 s and more. `allow` **with `updatedInput` set to the payload's `tool_input`** approves it. With `updatedPermissions: [{"type":"setMode","mode":"auto","destination":"session"}]` the next `Write` ran with no prompt and every later payload carried `permission_mode: auto`, exactly what the pane's option 1 gives. See [plan approval](#plan-approval). |
| 2 | Can an answer send annotations (notes, chosen preview) back in `updatedInput`? | **Yes.** `updatedInput.annotations = {"<question>": {"notes": "...", "preview": "..."}}` reaches the model as `selected preview: ... notes: ...` after the answer, and is in `PostToolUse` `tool_response.annotations` verbatim. A pane-answered single-select on an option that has a `preview` fills `annotations` itself: `{"<question>": {"preview": "<that option's preview>"}}` (no `notes` key without typing notes), so compound should send the chosen option's `preview` as the pane would. |
| 3 | The exact string a pane-answered multi-select gives in `PostToolUse` `tool_response.answers`, labels with commas included. | **Labels joined by `", "`; a label that contains `", "` is wrapped in double quotes, with any inner `"` backslash-escaped. A label with a bare comma and no space (`A,B`) is NOT quoted.** See [the multi-select string](#the-multi-select-string). |
| 4 | Does `PermissionRequest` fire for `AskUserQuestion` / `ExitPlanMode` / `Bash` under `auto`, `acceptEdits`, `bypassPermissions`? | **`AskUserQuestion` and `ExitPlanMode` fire in all three. `Bash` fires only when the mode would have asked:** yes in `acceptEdits` (a non-file command), no in `auto` (its classifier decides; a classifier-blocked command fires nothing at all) and no in `bypassPermissions`. Table [below](#when-permissionrequest-fires). |
| 5 | Does a Bash allow with `updatedPermissions` `addRules` from `permission_suggestions` behave as "always allow"? | **Yes, for that exact command, and only where the rule's source is enabled.** The suggestion is `{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"python3 -c \"print(11)\""}],"behavior":"allow","destination":"localSettings"}`: an exact-command rule, not a prefix. Echoed back, the repeat of that command no longer fires `PermissionRequest` and the rule is written to `.claude/settings.local.json`. With `--setting-sources project` (no `local`) it is written but not honoured. `destination: "session"` is honoured with nothing written. See [always allow](#always-allow). |
| 6 | Codex's `PermissionRequest` payload and decision schema. | **A binary was had (npm `@openai/codex` 0.163.0), but it was not run:** the package is arm64 and this Mac runs x86_64 shells, and a session needs an OpenAI login that was not used. The two JSON Schemas Codex embeds in its binary (`permission-request.command.input` / `.output`) were read out of it. They say: no `permission_suggestions`, `allow`/`deny` + `message` only, and `updatedInput` and `updatedPermissions` fail closed. See [Codex](#codex). |
| 7 | (Q-A6) The longest hook `timeout` Claude honours. | **No cap was found.** `timeout` 86400, 2147483, 2147484 (just past 2^31 ms) and 99999999999 seconds were each honoured: a 10 s hold ran to its answer and the agent continued, none fired early. A 700 s hold (past the 600 s default) with `timeout: 99999999999` was honoured: the hook answered at +700.1 s, the pane dialog stayed up the whole time and closed on the answer, and the agent continued. The control, `timeout: 20` with a 40 s hold, fell through to the pane dialog at 20 s, and the hook's late answer at 40 s did nothing (no `PostToolUse`). Not measured: holds longer than 700 s, and the dialog's own idle behaviour past that. |

## Plan approval

Three runs from `--permission-mode plan`, each asking for a plan then a `Write`, reply after a 3 to 4 s hold:

| hook reply | dialog | mode after | next `Write` |
|---|---|---|---|
| `allow` | **stays up** (no `PostToolUse`) | still `plan` | never ran |
| `allow` + `updatedInput` = `tool_input` | closes | `default` | asked (`PermissionRequest Write`) |
| `allow` + `updatedInput` = `tool_input` + `setMode acceptEdits` | closes | `acceptEdits` | ran, no prompt |
| `allow` + `updatedInput` = `tool_input` + `setMode auto` | closes | `auto` | ran, no prompt |
| pane, option 1 "Yes, and use auto mode" | closes | `auto` | ran, no prompt |

The pane's own list in 2.1.303 is `1. Yes, and use auto mode`, `2. Yes, manually approve edits`, `3. Tell Claude what
to change`. Option 1 lands in mode `auto`, not `acceptEdits`; the label depends on the account (auto mode offered),
which the payload does not say, so wsp should not assume the wording. The mapping that reproduces the pane:

| button | decision |
|---|---|
| approve, auto | `allow` + `updatedInput` + `updatedPermissions[setMode auto]` |
| approve, accept edits | `allow` + `updatedInput` + `updatedPermissions[setMode acceptEdits]` |
| approve, manual | `allow` + `updatedInput` (no `updatedPermissions`) |
| reject / "tell Claude what to change" | `deny` + `message` (the spike measured this) |

Without `updatedPermissions`, the mode after an approval is the mode **before** plan: `default` when the session
started in plan, but `auto` / `acceptEdits` / `bypassPermissions` after a `EnterPlanMode` from those (see the table
below), so the "manual" button does not force `default`; it keeps what the session had.

This also corrects the spike's section 2: "Approve = `allow`" is wrong for `ExitPlanMode`. The tools that need a
person (`AskUserQuestion`, `ExitPlanMode`) accept a hook's `allow` only with an `updatedInput`; `Bash` does not
(a bare `allow` ran the command, in every trial below). For `AskUserQuestion` it was already true by construction,
because the answers ride in `updatedInput`.

## The multi-select string

Pane-answered, `PostToolUse` `tool_response.answers` (and `tool_input.answers`, identical) for a multi-select
question:

| options picked | string in `answers["<question>"]` |
|---|---|
| `Lint`, `Test, and more, ok` | `Lint, "Test, and more, ok"` |
| `Say "hi", ok`, `A,B` | `"Say \"hi\", ok", A,B` |

So the join is `", "`; an element is quoted when it contains `", "` (a bare `A,B` was not; a label with a `"` and no
comma-space was not tried); inside the quotes `"` is `\"`. That is not safely invertible (`A,B` beside another label is `A,B, C`), so **do not split it**:
for display print it as is, and to recover which options were chosen match the known option `labels` against it in
order. A single-select answer is the bare label, unquoted even with a comma (`Yes, please`). A free-text "Other" is the typed text (the spike).
What the hook sends back is not parsed by Claude Code, so compound may use the same format (recommended: yes, for
one reading of the transcript) or any string the agent will understand; the spike's `Lint, Format` reached the
model verbatim.

## When PermissionRequest fires

One session per mode, a reply scripted for every tool so nothing waits. `Bash` is `python3 -c "print(42)"`; plan
mode is entered with `EnterPlanMode` so `ExitPlanMode` has something to leave.

| mode | `Bash` | `AskUserQuestion` | `ExitPlanMode` | `EnterPlanMode` | mode after `ExitPlanMode` |
|---|---|---|---|---|---|
| `default` | fires (spike) | fires (spike) | fires (this run, from `plan`) | not tried | `default` |
| `acceptEdits` | **fires** | fires | fires | no | `acceptEdits` |
| `auto` | **no** | fires | fires | no | `auto` |
| `bypassPermissions` | **no** | fires | fires | no | `bypassPermissions` |

`PreToolUse` and `PostToolUse` fire in every mode for every row, so they still carry the call when
`PermissionRequest` does not. In `auto` three commands were tried that the mode's classifier should not like
(`git push --force origin main` in a directory with no repo, a `curl -X POST --data-binary @/etc/hosts` to a dead
local port, and a harmless `rm -rf ./nonexistent`). The first two produced `PreToolUse` and **nothing else** (no
`PermissionRequest`, no `PostToolUse`: blocked); the third ran. A hook cannot see, let alone override, an `auto`
block. So a wsp hold for Bash is a `default` / `acceptEdits` / `plan` thing; in `auto` and `bypass` the question
never exists, and only the two interaction tools reach compound.

A `bypassPermissions` pty also shows a "WARNING: Claude Code running in Bypass Permissions mode" dialog at start
(`No, exit` / `Yes, I accept`) unless `skipDangerousModePermissionPrompt` is set; that is a start-up screen, not a
`PermissionRequest`, and compound's screen detection is what sees it.

## Always allow

`permission_suggestions` on a Bash request, default mode, copied back as `updatedPermissions`:

| command | suggestion |
|---|---|
| `python3 -c "print(11)"` | `addRules` `Bash(python3 -c "print(11)")`, `allow`, `destination: localSettings` |
| `ls /usr` | `addRules` `Read(//usr/**)`, `allow`, `destination: session` (the command read a directory outside the project) |
| (spike, a `/tmp` write) | `addDirectories` + `setMode acceptEdits`, both `session` |

| trial | repeat of the same command |
|---|---|
| sources `project,local`, suggestion as sent | **no `PermissionRequest`**; rule appended to `.claude/settings.local.json` |
| sources `project` only, suggestion as sent | **asks again** every time; rule is still written to `settings.local.json` |
| suggestion with `destination: "session"` | **no `PermissionRequest`**; nothing written to disk |
| a different argument (`print(12)`) | asks (the rule is the whole command line) |

So "always allow" is the exact command, for the life of the destination, and only when that settings source is
loaded; a person's normal sources (user, project, local) include `local`. `session` is the safe choice for a
button: it expires with the agent and writes no file in the user's repo. A suggestion is the dialog's own idea of
the rule, the shape of which depends on the command, so compound should show `ruleContent` and `destination`
before offering it, and send nothing the payload did not offer.

## Codex

Codex CLI 0.163.0 (`npm pack @openai/codex@0.163.0-darwin-arm64`, unpacked into its own directory, never run:
the binary is arm64 and this Mac reports x86_64, and a session would need credentials, which were not used).
The schemas are embedded in the binary as JSON text and read with `strings`, so this is the shape Codex accepts, not
a payload seen on the wire.

**Input** (hook stdin), `additionalProperties: false`:

- required: `session_id`, `cwd`, `hook_event_name` (`"PermissionRequest"`), `model`, `permission_mode`
  (`default | acceptEdits | plan | dontAsk | bypassPermissions`), `tool_name`, `tool_input` (any), `transcript_path`
  (string or null), `turn_id`;
- optional: `agent_id`, `agent_type`.
- no `permission_suggestions`, no `tool_use_id`.

**Output** (stdout JSON), `additionalProperties: false`:

- `hookSpecificOutput`: `{hookEventName: "PermissionRequest", decision: {behavior: "allow" | "deny", message?: string}}`
- top level: `continue` (default true), `stopReason`, `suppressOutput`, `systemMessage`.
- `decision.updatedInput`, `decision.updatedPermissions` and `decision.interrupt: true`: the schema calls each
  "reserved for a future ..." and says the hook **fails closed if the field is present**. Exit code 2 with a reason
  on stderr is also a denial ("PermissionRequest hook exited with code 2 but did not write a denial reason to
  stderr" is its error when the reason is missing).

Consequence for the build: a Codex adapter can **allow or deny with a sentence** a tool-permission request, and
nothing more. No answers into a question tool (there is no `AskUserQuestion` equivalent in the schema), no
`setMode`, no "always allow". The `choices` field for Codex is `kind: permission` with an Allow and a Deny button
only, and its hook must never set `updatedInput` or `updatedPermissions`. The command-handler config in the binary
has `type`, `command`, `timeout`, `async`, `statusMessage` (and a Windows command); `hooks.json` appears in the binary
(`config.toml` `[hooks]` is the spike's documentation reading). Not seen: which `tool_name` values it sends, how a hook timeout resolves (fail open or
closed), and whether `permission_mode` ever reads `dontAsk` for a request it would ask for. Those need a live run
with an account; the row that adds the Codex adapter should start by capturing one payload.

## What this changes in the build rows

- **wsp-301 (the hook)**: `ExitPlanMode` and `AskUserQuestion` decisions must carry `updatedInput`; build the
  plan buttons from the table above; the mapping is pure and table-testable. A permission "always allow" button is
  `permission_suggestions[i]` echoed back as `updatedPermissions`, destination forced to `session` unless a person
  chooses otherwise. The hold's `timeout` has no practical ceiling (row 7): set it by what compound should wait,
  not by what Claude allows.
- **wsp-301 / close-on-pane-answer**: nothing new beyond the spike; the pane-answered `PostToolUse` carries the
  string above, kept opaque.
- **compound rows**: `Wants::Choice` for `auto` / `bypassPermissions` sessions only ever has `ask` and `plan` kinds;
  a `permission` want exists in `default`, `acceptEdits` and `plan`.
