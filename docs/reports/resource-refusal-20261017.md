# Durable resource refusal — verified slice, 0.0.1-dev

Storage/recovery supports an explicit refused raw-output suffix under a separate
exact journal policy. Production launch policy and live memory behavior are
unchanged. [ADR0013](../adr/0013-durable-resource-refusal-event.md) records the
single-record decision and why the older pending/two-sync proposal is unnecessary.

Fresh root checks pass217 workspace tests, strict all-target Clippy, formatting
and release bins/examples. After extending the existing process-level suffix
fixture from four to six cases, the targeted test also passes: it serves every
cold record for a resource refusal and a legacy-policy kind mismatch. The native
app was not rebuilt/retested for this host-only reader slice; its prior127-test
redraw/native-IME evidence remains separately source-bound.

Storage40 tests include five new refusal cases: exact maximum payload, partial
frame writes, CRC kind tampering, uncertain sync and write sealing/reopen with
an inserted valid suffix. Refusal returns a typed durable receipt only after
successful sync; future writes leave bytes and sequence unchanged. Host87 tests
include profile matching, first-event refusal, full/checkpoint-tail complete-state
and nonzero cold history/continuation, clean new-policy compatibility, exact
leased-cache cleanup and checkpoint refusal before a busy permit/capture.

Initial integration failed because replay-cache creation still accepted only
the old profile. Its create/marker gates now recognize the same exact policy
set as recovery, retaining lease/inode/unknown-file guards. The existing unsupported
profile diagnostic regression then caught changed wording; the meaningful
`incompatible terminal profile` cause was restored. Independent review found
the checkpoint watermark seam; typed receipt guards and journal sealing close
the normal writer path without a full-prefix scan. Those failures were fixed,
not relabeled as passing checks.

Reproduce:

```sh
cargo test --workspace
cargo test -p pigade-session-host --test recovery \
  semantically_invalid_crc_valid_tail_preserves_and_serves_last_good_prefix
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo build --release --workspace --bins --examples
```

No global reservation ledger, estimator, memory gate, resize outcome, live refusal
or replay-time resource admission is claimed. The new profile is used by recovery
fixtures only until those mechanisms are ready. Ongoing elapsed soak binaries
remain frozen and are not replaced by this build.
