# `durable-agent` — P1 acceptance criteria and edge-case checklist

Written **before** any code, per the execution doctrine in `docs/PLAN.md`.
Every edge case below is either a named test or a `// EDGE:` comment beside the
code that handles it. Tests are preferred; the table records which.

---

## 0. The guarantee, stated honestly

> **At-least-once execution with stable idempotency keys**, conditional on a
> durable journal and idempotent effects.

Not exactly-once, and the earlier drafts of this file that said otherwise were
wrong. Nothing that survives a process death can be exactly-once: at the instant
the process dies, whether the effect landed is a fact about the outside world
that the engine has no record of.

What the crate does provide:

1. the intention is durably recorded **before** the act;
2. a step interrupted between those two moments is executed again;
3. every attempt at one step is handed the same `effect_id` and byte-identical
   input;
4. replay *finishes what is recorded* — it never re-asks the model for an answer
   it already has, never re-decides a decision a human already made, and never
   loses one;
5. what it dispatches is what the model asked for: a step's tool and arguments
   are resolved back to the call in the current `model_response`, so a journal
   cannot keep a real call's id while substituting a different action;
6. decisions the engine makes at replay time are on the journal, not re-derived
   by asking a tool again — a recorded `ReplayPolicy::Halt` cannot be undone by
   a redeployment that answers `Retry`;
7. **a run whose journal replays can always be driven to a terminal state**, by
   `run`, `resume` or `cancel`.

Exactly-once **effects** are therefore achievable, by the consumer: upsert on
`effect_id`, and supply a journal whose `append` is really durable. Neither
condition is checkable from inside this crate, and both are load-bearing.

Three claims this crate specifically does **not** make:

* **Approval is not idempotency.** `requires_approval` decides *whether*
  something may happen, once, before it happens. An approved step is fenced and
  replayed exactly like an unapproved one, so an approved email can still go
  twice. Effects with no natural key need an outbox keyed on `effect_id`;
  `ReplayPolicy::Halt` is a guard rail that converts a silent double-send into a
  loud stop, not a fix.
* **Durability is the consumer's.** The crate does no fsync, opens no file,
  starts no transaction. `MemoryJournal` is a test double. An `append` that
  returns before the write is durable does not weaken the guarantee — it removes
  it.
* **Point 7 is conditional on the journal replaying.** A journal refused as
  `CorruptJournal` or `UnsupportedJournalFormat` cannot be cancelled either: the
  engine does not know which sequence to append at. That is a storage problem
  and the host owns it. The earlier draft of this file said "the run is never
  stranded" without the condition, and — until `Engine::cancel` existed — without
  it even being true: a tool fingerprint that changed *after* an approval
  committed left `run` answering `ToolChanged` and `resume` answering
  `AlreadyResolved`, forever.

### What replay trusts

Replay takes the journal's **facts** as given — what the model said, what a
human decided, what a tool returned — because those *are* the run's history; a
host that cannot trust its own journal's contents has a problem this crate
cannot detect from the inside. It does not take its **claims** as given, and the
line is whether a field can be compared against something the engine wrote
earlier or is merely recomputed from the payload asserting it. A recomputed
field is not a check at all: any self-consistent forgery satisfies it. That
distinction is what E63–E66 are about, and it is the question to ask of any
field added later.

---

## 1. Scope

A generic, model-agnostic, runtime-agnostic agent engine:

* three traits — `Llm`, `Tool`, `Journal`;
* an `Engine` that drives a tool-calling loop over them;
* a **journal-before-effect** protocol so a crashed run can be replayed without
  duplicating side effects — given a durable journal and idempotent effects;
* a human-approval gate that owns the tool loop, with every resolution bound to
  the step it settles;
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
| A1 | Crate builds clean | `cargo build -p durable-agent` |
| A2 | No clippy warnings anywhere, including tests | `cargo clippy -p durable-agent --all-targets -- -D warnings` |
| A3 | Formatted | `cargo fmt --check` |
| A4 | All unit tests and doctests green | `cargo test -p durable-agent` |
| A5 | Rustdoc builds with no warnings | `cargo doc -p durable-agent --no-deps` |
| A6 | The above pass **twice consecutively** | run the block twice |
| A7 | Dependency list is small and boring | manual: no HTTP, no DB, no provider SDK, no async runtime in `[dependencies]` |
| A8 | Engine is `Send + Sync + 'static` and works behind `Arc` | `engine_is_send_sync_and_static` compile-time assertion test |
| A9 | Public contract documented | crate-level rustdoc + `README.md`; README example compiles as a doctest |
| A10 | The documented guarantee is exactly what the code delivers — no more | §0 of this file, the crate docs, and `README.md` all say "at-least-once, conditional"; every condition names the test that pins it. Two reviews have now found documentation claiming more than the code delivered (`requires_approval` re-checked on every dispatch; "the run is never stranded"), so treat overclaiming as its own defect class: each claim below either names a test or names its condition |

### State machine (must match `PLAN.md` §Agent runtime exactly)

```
queued → running → done | failed
running → pending_approval --approve--> queued → running
                           --skip----->  skipped
                           --expire--->  expired
running → waiting(wake_at) --timer----> running
any non-terminal state     --cancel---> failed { cancelled }
```

`queued` is the host's state before it calls `Engine::run`; the SDK models the
rest. Every transition has a test. `cancel` is `Engine::cancel`, added by the
second review (§0, point 7); it reaches the existing `failed` tag with
`FailureReason::Cancelled` rather than adding a status the NADE schema's check
constraint does not have.

### Journal-before-effect protocol (must match `PLAN.md` §Exactly-once)

1. append `step_started { step_seq, tool, args, args_hash, effect_id }`, let it commit;
2. execute the tool — effects use `effect_id = uuid5(run_id ‖ step_seq)`, exposed
   publicly as `durable_agent::effect_id(run_id, seq)`;
3. append `step_done { step_seq, result }`.

Replay: a step with `step_done` is skipped; a step with `step_started` and no
`step_done` is **re-executed** (safe *only* because the effect id is
deterministic and consumers upsert on it).

Replay also finishes what is recorded rather than restarting it: a committed
final model response completes the run without a new turn; a committed
Skip/Expire reaches `skipped`/`expired`; a committed `cap_breached` fails the
run without journalling a second breach. An absent `run_ended` means "finish
what is recorded", not "start again".

Replay validates before it interprets:

* the run's `JOURNAL_FORMAT` is one this build writes — checked first, before
  any other payload is decoded, so an incompatible journal is
  `UnsupportedJournalFormat` and not a serde failure at whichever field happens
  to disagree;
* sequences start at 1 and increase by exactly one; the first entry is the only
  `run_started`; nothing follows `run_ended`;
* **the tool and arguments of an opening entry are the ones the model actually
  asked for**, resolved back to the call in the *current* `model_response` —
  not merely to a call id seen at some point in the run;
* the turn order holds: no `model_response` abandons a step that started and did
  not finish, and nothing follows the turn that answered the run;
* every `step_seq` names a step really opened at that sequence; every
  `effect_id` equals `effect_id(run, step_seq)` and every `args_hash` equals
  `args_hash(args)`; a repeated `step_started` agrees with the original in every
  field but `attempt`; an approval is resolved at most once;
* no entry claims a `created_at` further ahead of the local clock than
  `max_journal_clock_drift`, no step claims to have opened after the entry
  recording it, and no approval expires before it was requested.

Anything else is `CorruptJournal`, refused before a step can reach dispatch
under an arbitrary or colliding effect id — or under a tool the model never
named.

Every dispatch, including a re-dispatch after a crash, additionally checks: the
tool's fingerprint still matches the one the step was opened under; a step
opened gated still holds a recorded `Approve`, and a step opened ungated is not
one the current build would gate; and the step's recorded `ReplayPolicy` (which
the current build may make stricter, never looser) permits the retry.

---

## 3. Edge-case checklist

**Status: all 76 pass** (103 tests in `cargo test`, of which the extra ones are
controls and unit tests for the helpers). Every row is a test; there are no
`// EDGE:`-only cases, though the code also carries `// EDGE:` comments at each
handling site.

E47–E62 came from an adversarial review of the first green build. Ten of them
were real protocol defects, three critical; the review is why §0 of this file
now says "at-least-once" where it used to say "exactly-once".

E63–E76 came from a second review of the fixes. One was critical and new — a
journal could keep a real model call's id while substituting the tool and the
arguments, and everything else replay checked was recomputed from the forgery,
so it passed. The rest are the same question asked of the remaining fields, plus
three gaps the fixes themselves opened: a decision taken at replay time that was
recorded nowhere, a permanent refusal with no way out, and a format change that
made older journals unreadable by accident rather than by policy.

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
| E47 | **Stale approval delivered after the run reached a *second* approval** | the duplicate is refused as `AlreadyResolved`; the later step does **not** execute | `stale_approval_does_not_approve_a_later_step` |
| E48 | Resolution naming a step that is neither pending nor decided | `Error::StepMismatch`, journal untouched | `resolution_for_an_unrelated_step_is_rejected` |
| E49 | Stale `Timer` delivered after the run parked on a *second* wait | refused as `StepMismatch`; the later wait is not cut short | `a_stale_timer_does_not_wake_a_later_wait` |
| E50 | **Crash after the final model response, before `run_ended`** | the run finishes from the recorded response; the model is not re-asked and its new answer cannot execute anything | `crash_after_final_model_response_finishes_from_the_journal` |
| E51 | **Crash between `step_done` and `run_waiting`** | the wait survives — it is committed *on* `step_done`, not in a separate entry | `crash_between_step_done_and_run_waiting_keeps_the_wait` |
| E52 | **Committed Skip with no `run_ended`** | `run` carries it to `skipped`; previously no public API could finish the run | `committed_skip_without_run_ended_still_completes` |
| E53 | Committed Expire with no `run_ended` | `run` carries it to `expired` | `committed_expire_without_run_ended_still_completes` |
| E54 | Committed `cap_breached` with no `run_ended` | `run` fails with that breach and does not journal it twice | `committed_cap_breach_without_run_ended_still_completes` |
| E55 | Recorded approval, crash before the tool ran | `resume` reports `AlreadyResolved`; `run` alone completes the run | `run_alone_completes_a_run_whose_approval_is_already_recorded` |
| E56 | Tool implementation changed under an open step | `Error::ToolChanged`; the new implementation never runs under the old step's `effect_id` | `a_changed_tool_cannot_execute_an_open_step` |
| E57 | Tool that **now** requires approval, over a step opened ungated | refused, not executed — the case that matters most | `a_tool_that_now_requires_approval_is_not_executed_ungated` |
| E58 | Non-idempotent tool interrupted mid-effect | not blind-retried; run fails `AmbiguousEffect` with the `effect_id` quoted | `a_non_idempotent_tool_is_not_blind_retried` |
| E59 | Two attempts at one step | identical `run_id`, `step_seq`, `effect_id`, `opened_at`, args; only `replay` differs | `every_attempt_at_a_step_sees_identical_input` |
| E60 | Clock moving backwards under an expiry check | `now` is floored by the newest journal entry, so a stale approval cannot be revived | `expiry_is_floored_by_the_newest_journal_entry` |
| E61 | Malformed journal (14 shapes: gaps, wrong first entry, foreign/colliding `effect_id`, mismatched `args_hash`, `step_seq` ≠ opening seq, disagreeing retry, orphan/duplicate `step_done`, post-terminal entry, unpaired wake, turn skew, phantom call, unknown kind) | `CorruptJournal` before anything dispatches | `src/tests/integrity.rs`, 15 tests plus a healthy control |
| E62 | Test double modelling non-idempotency as normal | `CountingTool` now returns byte-identical results across attempts; the anti-pattern lives in `DriftingTool`, which exists to be caught | `every_attempt_at_a_step_sees_identical_input` |
| E63 | **A journal reusing a real call id under a different tool** | refused: an opening entry's tool must be the one the model asked for. `effect_id` and `args_hash` are recomputed from the payload, so they do not catch it | `a_forged_step_cannot_substitute_the_tool_the_model_asked_for` |
| E64 | **The same, substituting only the arguments** | refused: the arguments must be the ones the model asked for | `a_forged_step_cannot_substitute_the_arguments_the_model_asked_for` |
| E65 | The same on the approval path, so a human is shown an action no model turn requested | refused before the approval card can be built | `a_forged_approval_cannot_substitute_the_tool_the_model_asked_for` |
| E66 | A `model_response` that abandons a step which started and never finished | refused — replay used to accept it and silently drop a step whose effect may exist | `a_model_turn_that_abandons_an_unfinished_step_is_refused` |
| E67 | A `model_response` after the turn that answered the run | refused; the answer ends the run | `a_model_turn_after_the_run_answered_is_refused` |
| E68 | An opening entry naming no model call at all | refused (pin: this already held) | `an_opening_entry_with_no_model_turn_at_all_is_refused`, control: `a_faithful_journal_still_replays_and_runs_the_step` |
| E69 | `requires_approval` on a re-dispatch of a step that was **already** gated | consulted, not short-circuited; the docs claimed this and the code did not | `requires_approval_is_re_evaluated_before_every_dispatch` |
| E70 | A recorded `ReplayPolicy::Halt`, under a build that now answers `Retry` | the halt holds: the policy is journaled when the step opens | `a_recorded_halt_survives_a_tool_that_later_permits_a_retry` |
| E71 | A step opened under `Retry`, under a build that now answers `Halt` | still refused — the current policy may only make the decision stricter | `a_tool_that_now_forbids_a_blind_retry_is_not_retried` |
| E72 | **Tool fingerprint changes after the approval committed** | `run` and every `resume` are dead ends by design, and `Engine::cancel` ends the run without executing anything | `a_run_whose_tool_changed_after_approval_can_still_be_ended` |
| E73 | `cancel` as a general kill switch, and on a run that never started | ends a paused run without executing; idempotent; refuses an unknown run | `cancelling_a_paused_run_executes_nothing_and_sticks`, `cancelling_a_run_that_never_started_is_refused` |
| E74 | One entry stamped far in the future | refused, rather than displacing the run's clock floor for good; ordinary skew still accepted | `an_entry_stamped_far_in_the_future_is_refused`, `an_entry_a_little_ahead_of_the_local_clock_is_accepted` |
| E75 | A step claiming to have opened after the entry recording it, or an approval expiring before it was requested | refused | `a_step_opened_after_the_entry_that_records_it_is_refused`, `an_approval_that_expired_before_it_was_requested_is_refused` |
| E76 | A journal in an older or newer `JOURNAL_FORMAT` | `Error::UnsupportedJournalFormat`, checked before any other payload is decoded — not a serde failure reported as a corrupt journal | `a_journal_from_an_older_format_is_refused_with_a_typed_error`, `a_journal_from_a_newer_format_is_refused_with_a_typed_error` |

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
  upsert store; optional approval gate, declared `version`, and `ReplayPolicy`),
  `PanicTool`, `HugeTool`, `FailingTool`, `WaitTool`, `DriftingTool`.

`CountingTool` returns **byte-identical results across attempts**. It used to
return `writes: 1` then `writes: 2`, which modelled a tool that breaks the
idempotency contract as though that were fine — and taught every reader the same
thing. The write count is still observable through `effects()`, where a test can
read it without the tool having to leak it into its own output. `DriftingTool`
is the anti-pattern, kept deliberately so a test can show what it costs.

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
9. **Reversed by the first review, completed by the second.** A step's gate
   decision is journaled when the step opens, but `Tool::requires_approval` **is**
   re-consulted before every dispatch, including a re-execution. The original
   reasoning ("the fence has committed, so pausing is theatre") only covers the
   case where the answer is unchanged. It misses the one that matters: a
   redeployment where the tool now requires approval, under which the old
   ungated step would execute a call the current build says needs a human. The
   engine refuses that with `Error::ToolChanged` rather than executing or
   silently re-gating, because it cannot mint a new `effect_id` for a step whose
   fence has already committed.
   The first fix wrote the check as `if !record.gated && t.requires_approval(..)`,
   which short-circuits for a step that was *already* gated — so the claim above
   was true only by accident, and only in one direction. It is now unconditional
   and both halves are stated: a step never executes ungated if the tool now
   requires approval, **and** a step opened gated must still hold a recorded
   `Approve`, which does not consult the tool at all.
10. A resolution carries the `step_seq` it settles. `Resolution` is therefore a
   struct-variant enum rather than a plain tag. Without the binding, a delayed
   duplicate of an old decision and a fresh decision about a *new* pending step
   are the same value, and the engine settles the wrong one.
11. An already-decided step answers `Error::AlreadyResolved` rather than an `Ok`
   no-op. A typed error is what "reports already resolved" means, and it
   composes with `API.md` §7's `409 token_consumed`, which the client also
   treats as success. The cost is that a worker retrying `resume` must fall back
   to `run`; that path is tested (`run_alone_completes_…`) so the run is never
   stranded.
12. `step_done` carries the `wake_at`, and `run_waiting` is demoted to a marker
   for readers. One committed entry cannot lose half of itself; two can.
13. `ReplayPolicy::Halt` ends the run `Failed` rather than raising a bare `Err`.
   A failed run is terminal, carries the `effect_id` to the host, and cannot
   strand the run — a repeated `Err` would. It is journaled as `cap_breached`
   before `run_ended`, so a lost terminal append still leaves the refusal on
   record.
14. **The journal format is versioned and not migrated.** `run_started` carries
   `journal_format`; a mismatch is `UnsupportedJournalFormat`. The alternative —
   serde defaults for the fields older writers never recorded — means inventing
   a step's open time and the replay policy a decision was taken under, which
   are exactly the facts the protocol exists to pin down. A hard break that says
   so is better than a soft one that guesses. The crate is unpublished, so this
   costs nothing today; the point is that the policy is now deliberate.
15. **`Engine::cancel` exists, and "re-approve under the new implementation"
   does not.** Every refusal in this crate is permanent by design, which needs
   an escape hatch; cancelling is the host saying "then it does not happen", and
   recording that. Rebinding a step to a new implementation would run a
   different action under an `effect_id` a human authorised for something else,
   so the answer is: cancel, and start a run the human can approve on its own
   terms.
16. **A cancelled run is `Failed { Cancelled }`, not a new `RunStatus`.** The
   status tags match the `agent_runs.status` check constraint in the NADE
   schema; adding one would be a migration this lane does not own, and the
   distinction a host actually needs is in `FailureReason`.
17. **The clock floor is bounded in both directions.** It exists so a backwards
   clock cannot revive an expired approval, and it is built out of journal
   timestamps — so an entry from the future displaces the run's clock
   permanently. `max_journal_clock_drift` bounds it from above. A faithful
   engine cannot produce such an entry, but the crate cannot assume a faithful
   journal implementation, which is the same reasoning as everywhere else here.
18. **A step's replay policy is journaled, and re-asking the tool may only make
   the decision stricter.** "This must not run twice" is a fact about an effect
   that may already exist; a later build answering `Retry` has not learned
   anything that unmakes it.
