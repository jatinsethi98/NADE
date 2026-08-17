# `nade-agent-sdk` — P1 acceptance criteria and edge-case checklist

Written **before** any code, per the execution doctrine in `docs/PLAN.md`.
Every edge case below is either a named test or a `// EDGE:` comment beside the
code that handles it. Tests are preferred; the table records which.

---

## 1. Scope

A generic, model-agnostic, runtime-agnostic agent engine:

* three traits — `Llm`, `Tool`, `Journal`;
* an `Engine` that drives a tool-calling loop over them;
* a durable **journal-before-effect** protocol so a crashed run can be replayed
  without duplicating side effects;
* a human-approval gate that owns the tool loop;
* caps (steps, tokens, tool-result bytes) that fail the run loudly.

**Out of scope for P1** (P4 owns them): the Postgres journal driver, real LLM
adapters (OpenAI-compatible / Anthropic), any HTTP client, any database code,
any streaming.

**Hard rule:** zero NADE types. The crate must compile knowing nothing about
email. No `nade-server` dependency, ever.

---

## 2. Acceptance criteria

| # | Criterion | How it is checked |
|---|---|---|
| A1 | Crate builds clean | `cargo build -p nade-agent-sdk` |
| A2 | No clippy warnings anywhere, including tests | `cargo clippy -p nade-agent-sdk --all-targets -- -D warnings` |
| A3 | Formatted | `cargo fmt --check` |
| A4 | All unit tests and doctests green | `cargo test -p nade-agent-sdk` |
| A5 | Rustdoc builds with no warnings | `cargo doc -p nade-agent-sdk --no-deps` |
| A6 | The above pass **twice consecutively** | run the block twice |
| A7 | Dependency list is small and boring | manual: no HTTP, no DB, no provider SDK, no async runtime in `[dependencies]` |
| A8 | Engine is `Send + Sync + 'static` and works behind `Arc` | `engine_is_send_sync_and_static` compile-time assertion test |
| A9 | Public contract documented | crate-level rustdoc + `README.md`; README example compiles as a doctest |

### State machine (must match `PLAN.md` §Agent runtime exactly)

```
queued → running → done | failed
running → pending_approval --approve--> queued → running
                           --skip----->  skipped
                           --expire--->  expired
running → waiting(wake_at) --timer----> running
```

`queued` is the host's state before it calls `Engine::run`; the SDK models the
rest. Every transition has a test.

### Journal-before-effect protocol (must match `PLAN.md` §Exactly-once)

1. append `step_started { step_seq, tool, args, args_hash, effect_id }`, let it commit;
2. execute the tool — effects use `effect_id = uuid5(run_id ‖ step_seq)`, exposed
   publicly as `nade_agent_sdk::effect_id(run_id, seq)`;
3. append `step_done { step_seq, result }`.

Replay: a step with `step_done` is skipped; a step with `step_started` and no
`step_done` is **re-executed** (safe because the effect id is deterministic and
consumers upsert on it).

---

## 3. Edge-case checklist

**Status: all 46 pass.** Every row is a test; there are no `// EDGE:`-only
cases, though the code also carries `// EDGE:` comments at each handling site.

| # | Edge case | Expected behaviour | Covered by |
|---|---|---|---|
| E1 | Empty tool list | `ChatRequest.tools` is empty; run still completes; a tool call becomes an unknown-tool error | `empty_tool_list_completes` |
| E2 | Empty model output | `Done { output: None }`, not a failure | `empty_model_output_is_done_with_no_output` |
| E3 | Whitespace-only model output | trimmed to nothing → `output: None`; raw text still journaled | `whitespace_model_output_is_trimmed_to_none` |
| E4 | Unicode in tool args | survives journal round-trip byte-identical; `args_hash` stable across processes | `unicode_args_and_results_round_trip` |
| E5 | Unicode in tool results | same; truncation never splits a UTF-8 code point | `unicode_result_truncation_is_char_safe` |
| E6 | Crash between `step_started` commit and the effect | resume re-executes; **exactly one** execution total | `crash_between_step_started_and_effect_executes_once` |
| E7 | Crash between the effect and `step_done` | resume re-executes (2 executions) but the effect id is identical, so the upsert store holds **one** row | `crash_between_effect_and_step_done_still_one_effect` |
| E8 | Crash before `step_started` commits | nothing ran; resume starts the step cleanly, one execution | `crash_before_step_started_commit_runs_once` |
| E9 | Duplicate `run` of an in-flight run | replays the journal, never re-appends `run_started` | `duplicate_run_replays_instead_of_restarting` |
| E10 | Duplicate `resume` of a finished run | no-op; returns the recorded terminal outcome; journal length unchanged | `duplicate_resume_of_finished_run_is_noop` |
| E11 | Approval "token" replayed (double approve) | second `resume(Approve)` is a no-op, tool executes once | `double_approve_executes_tool_once` |
| E12 | Approval expired | `resume(Approve)` after `approval_ttl` → `Expired`, tool never runs | `approve_after_expiry_expires_run` |
| E13 | Clock skew on expiry | expiry uses `expires_at + clock_skew_leeway`; inside the leeway an approve still works | `approve_within_clock_skew_leeway_succeeds` |
| E14 | Step cap hit exactly at the boundary | `max_steps` steps succeed → `Done` | `step_cap_boundary_exactly_at_limit_succeeds` |
| E15 | One step over the cap | `Failed { StepCapExceeded }`, breach journaled as `cap_breached` | `step_cap_one_over_limit_fails` |
| E16 | Token budget hit exactly | spend == budget → `Done` | `token_budget_boundary_exactly_at_limit_succeeds` |
| E17 | Token budget exceeded by the response just received | `Failed { TokenBudgetExceeded }`, breach journaled | `token_budget_one_over_limit_fails` |
| E18 | Token budget exhausted before the next turn | pre-call check fails the run rather than calling the model | `token_budget_blocks_next_turn_when_exhausted` |
| E19 | Tool panics | caught; structured `tool_panicked` error fed back to the model; run continues; counts a step | `panicking_tool_is_caught_and_reported` |
| E20 | Tool returns `Err` | structured `tool_error` fed back to the model; run continues | `tool_error_is_reported_to_model` |
| E21 | Tool returns 10 MB | truncated with an explicit marker; journal entry stays bounded | `oversized_tool_result_is_truncated_with_marker` |
| E22 | Model calls an unknown tool | structured `unknown_tool` error (with the allowlist) fed back; counts a step | `unknown_tool_call_is_structured_error_and_counts_a_step` |
| E23 | Model loops on the same tool forever | step cap terminates it | `model_looping_forever_is_stopped_by_step_cap` |
| E24 | Approval-requiring tool cannot run without a resolution | `run` twice → still `PendingApproval`, zero executions | `approval_gated_tool_never_runs_without_resolution` |
| E25 | Skip path | `resume(Skip)` → `Skipped`, zero executions | `skip_path_ends_skipped` |
| E26 | Expire path | `resume(Expire)` → `Expired`, zero executions | `expire_path_ends_expired` |
| E27 | Approve path round-trip | `run` → `PendingApproval` → `resume(Approve)` → `Done` | `pause_resume_round_trip_approve_completes` |
| E28 | `resume` with no pending approval | typed `Error::NoPendingApproval`, journal untouched | `resume_without_pending_approval_errors` |
| E29 | `resume(Timer)` on a run that is not waiting | typed `Error::NotWaiting` | `timer_resume_when_not_waiting_errors` |
| E30 | Waiting state round-trip | tool parks the run → `Waiting { wake_at }` → `resume(Timer)` → `Done` | `waiting_then_timer_resumes_and_completes` |
| E31 | Journal seq conflict (two workers on one run) | `Error::SeqConflict`, second writer loses | `seq_conflict_is_detected` |
| E32 | Duplicate tool names at construction | `Error::DuplicateTool` from `Engine::new` | `duplicate_tool_names_rejected` |
| E33 | Model emits duplicate/empty tool-call ids | engine normalises ids **before** journaling, so replay is stable | `duplicate_and_empty_call_ids_are_normalised` |
| E34 | Multiple tool calls in one turn, approval in the middle | earlier calls execute, the run pauses, later calls run only after approval | `multi_call_turn_pauses_midway_and_resumes_in_order` |
| E35 | `stop_reason: ToolUse` with no tool calls | treated as a final answer, not an infinite loop | `tool_use_stop_reason_without_calls_ends_the_run` |
| E36 | `effect_id` is a stable public contract | golden vector pinned in a test | `effect_id_golden_vector` |
| E37 | Journal layout is a stable contract | exact `(seq, kind)` list pinned | `journal_layout_is_stable` |
| E38 | Approval-gated step keeps the effect id it was quoted at approval time | `ApprovalRequest.effect_id == effect_id(run_id, step_seq)` == the id the tool sees | `approved_step_keeps_its_quoted_effect_id` |
| E39 | Very large tool **arguments** | not truncated (they bound the model's own output) — documented `// EDGE:` | `// EDGE:` in `engine.rs` |
| E40 | `run` on a run already `pending_approval` / `waiting` / terminal | returns the recorded outcome, appends nothing | `run_on_paused_run_is_noop` |
| E41 | Non-object tool arguments (model emits a scalar/array) | passed through untouched; hashing and journaling still work | `non_object_arguments_are_passed_through` |
| E42 | `approval_ttl: None` | approvals never expire; a late approve still works | `approval_without_ttl_never_expires` |
| E43 | Absurd `approval_ttl` / `clock_skew_leeway` | clamps at the end of representable time; `chrono`'s `Add` would otherwise **panic** | `absurd_ttl_and_leeway_clamp_instead_of_panicking` |
| E44 | Every public type survives a JSON round trip | a host stores all of them; tags match the database columns | `conversation_types_round_trip`, `journal_payloads_round_trip`, `outcomes_and_resolutions_round_trip`, `run_id_is_transparent_on_the_wire` |
| E45 | Crash before `model_response` commits | the turn is bought again; no effect can have happened, so none can duplicate | `crash_before_model_response_commit_re_asks_the_model` |
| E46 | System prompt | prepended to every turn and recorded in `run_started` | `system_prompt_is_prepended_to_every_turn` |

E43 and E44 were found by the self-review pass, not the first draft: E43 was a
real panic (`DateTime + TimeDelta` panics on overflow, so a config value could
have taken a server down) and is now fixed by `engine::deadline`.

---

## 4. Named tests the parent asked for, mapped

| Required by the brief | Test name |
|---|---|
| pause/resume round-trip (approve resumes and completes) | `pause_resume_round_trip_approve_completes` |
| skip path ends `skipped` | `skip_path_ends_skipped` |
| expire path ends `expired` | `expire_path_ends_expired` |
| crash between `step_started` and the effect → one effect | `crash_between_step_started_and_effect_executes_once` |
| crash between the effect and `step_done` → still one effect | `crash_between_effect_and_step_done_still_one_effect` |
| duplicate `resume` of a finished run is a no-op | `duplicate_resume_of_finished_run_is_noop` |
| step cap boundary | `step_cap_boundary_exactly_at_limit_succeeds` / `step_cap_one_over_limit_fails` |
| token budget boundary | `token_budget_boundary_exactly_at_limit_succeeds` / `token_budget_one_over_limit_fails` |
| approval-requiring tool cannot execute without a resolution | `approval_gated_tool_never_runs_without_resolution` |
| oversized tool result truncated with a marker, entry bounded | `oversized_tool_result_is_truncated_with_marker` |
| unknown-tool call returned as a structured error, counts a step | `unknown_tool_call_is_structured_error_and_counts_a_step` |

---

## 5. Test doubles

Behind the **`testing` feature**, also auto-enabled under `cfg(test)`:
`#[cfg(any(test, feature = "testing"))] pub mod testing`. In-crate unit tests
therefore need no self-dependency, and downstream crates (P4's `nade-server`)
can opt in with `features = ["testing"]`.

* `ScriptedLlm` — hands out a `Vec<ChatResponse>` in order; **panics loudly**
  when the engine asks for a turn that was not scripted; records every request.
* `MemoryJournal` — `Arc<Mutex<HashMap<RunId, Vec<Entry>>>>`, enforces the
  `(run_id, seq)` primary key, and can fail an append at a chosen seq either
  **before** the entry commits or **after** it commits (the two distinct crash
  shapes).
* Tools — `EchoTool`, `CountingTool` (execution counter + `effect_id → count`
  upsert store, optional approval requirement), `PanicTool`, `HugeTool`,
  `FailingTool`, `WaitTool`.

---

## 6. Judgement calls to record in the report

1. `Engine` is generic over `Llm` and `Journal` but holds `Vec<Arc<dyn Tool>>` —
   a heterogeneous tool set cannot be a single type parameter.
2. `Journal::append` takes a fully-formed `Entry` whose `seq` the engine
   allocates; the journal enforces `(run_id, seq)` uniqueness. This mirrors the
   Postgres primary key in `PLAN.md` and gives free protection against two
   workers driving one run.
3. `step_started` carries the full `args`, not just `args_hash`, so replay is
   self-contained. `args_hash` is kept for integrity/debugging.
4. A step is identified by `step_seq` — the journal seq of the entry that
   *opened* it (`step_started` **or** `approval_requested`). `effect_id` is
   derived from that seq, so an approval quotes the final effect id up front.
5. LLM and journal I/O errors propagate as `Err` from `run`/`resume` (the host
   retries the job); only cap breaches produce `RunOutcome::Failed`.
6. `waiting(wake_at)` is entered by a tool returning `control::wait_until(..)` —
   an explicit, opt-in helper rather than a magic string.
7. Default `rustfmt` settings, no `rustfmt.toml`: the workspace has none, and
   adding one is not this lane's file to touch.
8. `EntryKind` keeps an `Other(String)` variant so a journal written by a newer
   version can be *read*, but replay refuses to interpret one — better a loud
   `CorruptJournal` than a silent misreading of a step's state.
9. A step's gate decision is made once, when the step is opened, and journaled.
   Re-execution after a crash does **not** re-consult
   `Tool::requires_approval`: the fence has already committed, so the effect may
   already exist, and pausing at that point would be theatre.
