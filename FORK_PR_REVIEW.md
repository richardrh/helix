# Upstream PR review for the steel-evil fork

Reviewed on 2026-10-12 against open `helix-editor/helix` PRs, using `gh pr view/checks/diff`.
Decision: cherry-picked #16290 as `659acf7bd` and #16274 as `1c5940029`, preserving original authors. Both are small regression fixes. The other candidates below remain deferred.

Selection constraint: prefer isolated core fixes over changes to typed commands, keymaps or application startup, where this fork adds Steel/Evil/MCP integration.
Both recommended PRs touch only `helix-core`.

## Recommended (merge candidates)

### 1. #16290: Do not hang when incrementing numbers with adjacent separators
- Head SHA: `a9f18acccbfa7ca5863c61233a3392453f8bcd49`
- Size: +20/-0, 1 file (`helix-core/src/increment/integer.rs`)
- Checks: all 8 pass (msrv, Grammar Lint, Lints, Test Suite on macOS, Ubuntu x64 and arm, Windows x64 and arm)
- Problem: pressing Ctrl-a on `9__99` (two underscores in a row) froze the editor. The separator re-insertion loop never moved forward (issue #16246).
- Fix: Helix now ignores such numbers and changes nothing, the same way it already handles leading or trailing `_`. A unit test is included.
- Risk: very low. It is an early return with a test.
- Note: this conflicts with #16360 and #16263, which edit the same function. Take only one of them.

### 2. #16274: fix(snippets): drop stale tabstop session instead of panicking
- Head SHA: `c72fb797acd4fe44c88278842da36103aa5d2642`
- Size: +31/-12, 1 file (`helix-core/src/snippets/active.rs`)
- Checks: none have run (first-time contributor, so the workflow needs approval)
- Problem: Helix crashed when you edited or deleted parts of a completed snippet (for example its parentheses) and then jumped to the next tabstop. The code reached `unreachable!("active snippet must be valid")` (issue #16271, still open).
- Fix: `primary_idx` now returns `Option`. When the selection has left the snippet, `next_tabstop` and `prev_tabstop` end the session instead of crashing. A regression test is included.
- Risk: low. Because CI has not run, run `cargo test -p helix-core` locally after cherry-picking.

## Deferred

| PR | Head SHA | Checks | Reason |
|---|---|---|---|
| #16363 Comment toggling with multi-byte tokens | `93d171769dba8cb59c035bf74e48767b1e69a578` | 7 pass, msrv fails | The msrv failure came from a git.sr.ht outage while fetching grammars, not from this PR. The fix is correct and tested but only matters for unusual comment tokens such as emoji. It can be merged later. |
| #16360 Increment hang plus other increment fixes | `b0d29d94beb81153b496b2d108621aeb51d7ca66` | all pass | A larger rewrite of separator handling that also fixes u128 wraparound. It overlaps with #16290. Choose it instead of #16290 if you want those extra fixes too. |
| #16263 Increment underscore hang and u128 wrap | `3746b4103ea84c658448ef28bc0454b8f8f6eef3` | none | Duplicates #16360 and has no CI run. |
| #16258 Prompt anchor clamp after deletions | `e63dd30465e912cf1fa1c31977e7e07c313fada5` | none | Fixes a real crash but changes many prompt functions and has no CI run. |
| #16278 Keymap sequence after the last window closes | `bc55a7035475fb85be7a8852003d066c22bef95a` | none | Touches the key-sequence path that other agents are editing. No CI run. |

Not considered because they are broad architectural changes: #16260 (debounced edit handler), #16252 (`editor.diff-base`), #16280 (flush writes before shell commands), #16294 (LSP refresh after config reload).

## Integration and validation

The following imports were applied successfully. `cargo test -p helix-core --lib`
passed all **170 tests**, including the imported regression cases, on macOS arm64
on 2026-10-12.

### Reproduction commands

```sh
git fetch https://github.com/helix-editor/helix pull/16290/head pull/16274/head
git cherry-pick a9f18acccbfa7ca5863c61233a3392453f8bcd49 c72fb797acd4fe44c88278842da36103aa5d2642
cargo test -p helix-core
```
