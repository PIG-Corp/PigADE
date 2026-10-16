# Checkpoint review — 0.0.1-dev

Review owner: `recovery_review`. Date: 2026-10-17. Final scoped retest: state.2 Term checkpoint with the profile updated by root; first-party version remains 0.0.1-dev. Read-only implementation review; only this report and the required append-only worklog are owned by this reviewer. Findings were sent to root immediately. The repository was being edited concurrently; locations and measured output below describe the `pigade-state.1` experiment unless an explicit retest says otherwise. No stack/MVP approval follows from passing tests.

## Code Review Summary

**Files reviewed:** 12 implementation files (checkpoint-related context): VTE `lib.rs`, `ansi.rs`, `params.rs`; Alacritty `term/checkpoint.rs`, `grid/checkpoint.rs`, `grid/storage.rs`; terminal `lib.rs`; session-host `checkpoint.rs`, `lib.rs`; storage `journal.rs`, plus the new Alacritty `term/checkpoint/bounded.rs` and `pools.rs`. `term/mod.rs`, `cell.rs`, selection/row definitions and tests were consulted as reference context.

**Total issues found:** 6. CRITICAL: 0; HIGH: 5; MEDIUM: 1; LOW: 0. All six exact reproduced defects were independently confirmed repaired during review. This does not close the separate full-allocation, performance, history, platform and crash-test acceptance gaps below. The IPC issue is a related scope crossing reported upward, rather than an implementation assignment.

**Recommendation:** COMMENT on the corrected experimental slice; the original REQUEST CHANGES findings below are now repaired for their exact reproductions. No approval of the stack or release is given. Keep the SPEC stage-0 gate open; do not accept a screen-equality suite as complete checkpoint certification.

## Spec compliance and root-cause guard

SPEC §§7–8/10/12/13 and ADR 0001/0002 were read. The source preserves important boundaries: only the host parses ANSI; journal append/fsync precedes model publication; imported historical models have `live=false`; snapshot identity/CRC/profile are checked; temporary file/fsync/rename/directory sync are present; raw journals remain intact. The Term/Grid/Parser DTOs cover the explicit semantic fields inspected, including both cursors, inactive state, REP, partial sequences and synchronized bytes.

The current tests are useful continuation evidence, but do not prove bounded decoding, all protocol inputs, publication crash boundaries, immutable cold-history indexing, long-history budgets, VM reboot or a 24-hour real-time soak. Those remain release/gate requirements, not optional follow-ups. No unsupported platform or CLI claim was made by this review.

Do not resolve the issues below by suppressing checkpoint errors, discarding hyperlinks/history, resetting the parser, silently using a different profile, or presenting an old frame as live. Serialization sharing/limits and candidate validation need repair at their primary contracts. Full replay is the documented explicit fallback, not justification to leave the fast path unbounded.

## Issues

### [HIGH] CP-01: valid OSC 8 output amplifies checkpoint encoding beyond its resource limit

**Location:** `crates/terminal/src/lib.rs:175` (`postcard::to_allocvec`); `vendor/alacritty_terminal/src/grid/checkpoint.rs:94` (`Vec<Row<Cell>>`); `crates/session-host/src/checkpoint.rs:26` (limit checked after allocation).

Cells reuse an `Arc<CellExtra>`/hyperlink in the live model, but ordinary serde serializes each occurrence independently. A valid OSC 8 URI is duplicated for every linked cell in the exported checkpoint. The host tests the 128 MiB limit only after serialization has allocated the complete result. Supported CLI output can therefore create an arbitrarily large allocation or make the actor enter permanent degraded state on checkpoint, despite a small durable journal and small live model.

**Executed reproduction:** geometry 24×80; URI `https://example.test/` followed by 32,768 `x`; 4,200 linked `A` characters. Raw input: **37,001 bytes**. Encoded checkpoint: **137,827,052 bytes**, exceeding 134,217,728-byte host limit. A separate load/probe process reported **143,468 KiB peak RSS**. This is valid supported OSC 8, not a forged checkpoint.

**Required repair:** a versioned checkpoint representation with deduplicated shared extras/hyperlinks, plus an encoding writer that enforces its byte/allocation budget while writing. Preserve the semantics and raw journal. Regression evidence must cover repeated URI sharing, distinct URI/ID identity, both grids, hot history, import and continuation, and a bounded failure for a genuinely oversized semantic state. **Fresh state.2 reproduction:** the same input produces an **87,259-byte** checkpoint; reimport, exact reexport, resize, alternate-screen roundtrip, REP and additional output agree. Root added a cap-before-write Postcard writer, and storage added shared extra/hyperlink pools. The specific repeated-URI amplification defect is resolved. A genuinely huge semantic state can still exceed explicit budgets; do not infer unlimited supported state or the complete global RSS budget from this case.

### [HIGH] CP-02: collection bounds are checked after their allocations

**Location:** `crates/terminal/src/lib.rs:182`; `vendor/alacritty_terminal/src/grid/checkpoint.rs:110`.

The encoded input cap does not bound the size of decoded Rust structures. `postcard::take_from_bytes` completes derived serde deserialization before `TermCheckpointV1::validate` checks row/cell counts. Tiny encoded empty rows have much larger in-memory `Row<Cell>` representations. Title, cell extras and other variable collections also need their own length/budget validation before allocation; outer file length is insufficient.

**Executed reproduction:** replace the active-grid rows in a structurally encoded envelope with **5,000,000** empty rows (`inner=[]`, `occ=0`). The checkpoint is only **10,003,355 bytes**. A fresh decode-only process reaches **171,336 KiB peak RSS** before returning `invalid grid history bounds`. Extrapolation to the accepted 128 MiB file cap is a risk estimate, not an executed OOM: tens of millions of row structs can consume gigabytes before rejection.

**Required repair:** bounded serde visitors or equivalent bounded decoding for every dynamic collection, rejecting its declared length before allocating. Tie active/inactive hot-history limits to the fixed terminal profile, not merely the generic one-million-row DTO cap. Cover valid boundary sizes and malformed lengths under a constrained-memory subprocess. **Fresh state.2 reproduction:** the corresponding malformed envelope is **10,003,357 bytes** and is rejected before the oversized row vector is materialized; a fresh decode-only process peaks at **13,892 KiB RSS**. Source review confirms count-first rows/cells and referenced pool limits, aggregate combining limits, bounded text/stacks, plus bounded VTE OSC/sync byte vectors. The exact five-million-empty-row allocation defect is resolved. **Remaining verification gap:** 16,777,216 cells per grid and two grids are still large bounds, and an end-to-end aggregate decoder/DTO/import allocation budget below the SPEC RSS budgets has not been demonstrated. Storage explicitly reported this limitation. A small rejected malicious input diagnostic is not proof of bounded production recovery memory.

### [HIGH] CP-03: imported invalid UTF-8 continuation state panics on the next byte — repaired during review

**Original location:** `vendor/vte/src/lib.rs:96` (then-current importer), continuation at original `:725`.

The importer checked only `partial_utf8_len <= 3`. It accepted `partial_utf8=[65,66,67,0]`, `partial_utf8_len=3`. Advancing by `X` then subtracts 3 from the first scalar's byte length 1, panicking with `attempt to subtract with overflow` in debug, and producing an invalid consumed count in release. This bypasses a fallible import boundary and can crash recovery instead of rejecting a malformed checkpoint.

**Executed original result:** standalone probe exit 101, panic at `advance_partial_utf8`. **Fresh source/fix check:** root added the requirement that a pending prefix be exactly one incomplete scalar (`valid_up_to == 0`, `error_len == None`) and parser state Ground. Rebuilding the same probe now rejects at import with `invalid parser checkpoint` before any continuation. This specific finding is **resolved in current inspected source**, subject to final integration.

**Regression suggestion:** malformed prefixes consisting of complete ASCII, a complete multibyte scalar, invalid leading/continuation bytes, invalid lengths and pending UTF-8 outside Ground must all return Err; every valid 1–3 byte incomplete scalar prefix must import and continue identically. Include a real checksum-valid composed envelope and follow-up tail, not only direct DTO validation.

### [HIGH] CP-04: the same OSC 8 amplification also makes live IPC snapshots unavailable

**Location:** `crates/terminal/src/lib.rs:287`; `crates/session-host/src/lib.rs:88`.

This crosses the checkpoint-only slice but is independent of repairing checkpoint wire encoding. `frame()` copies the URI into every visible cell; `send()` serializes the complete JSON value before checking the 8 MiB IPC cap. Valid linked terminal content therefore expands every render snapshot and can make List/Snapshot fail. Multiple clients also repeat the allocation work. A bounded transport cap applied after full serialization does not preserve a usable snapshot contract.

**Source proof and executed reproduction:** the CP-01 valid input is used with `serde_json::to_vec(&terminal.frame())`; the measured frame JSON is **61,871,281 bytes**, exceeding the 8,388,608-byte IPC cap. No actual native UI was certified by this probe.

**Required repair:** a shared hyperlink representation in the render DTO and bounded transport serialization/admission. Keep all supported linked cells and make oversize results explicit, without silently dropping frame fields. Root repaired the render DTO with a hyperlink pool and bounded JSON serialization. **Fresh reproduction:** the same input now produces **234,461-byte** frame JSON, below the 8 MiB cap. This specific repeated-URI case is resolved; malformed/unique-URI aggregate bounds remain part of final integration checks. This reviewer did not implement the fix.

### [MEDIUM] CP-05: watermark rejection skips older valid checkpoints

**Location:** `crates/session-host/src/lib.rs:211`; `crates/session-host/src/checkpoint.rs:84`.

`latest()` chooses the newest file that decodes, but journal-offset eligibility is checked later in `restore`. If that check fails, restore goes directly to full replay instead of considering the next older checkpoint. An older intact checkpoint can preserve state past a corrupt journal prefix even when a newer snapshot's watermark is no longer usable. The current split candidate validation can therefore return less recoverable history than an existing valid recovery path.

**Executed reproduction:** write valid cp1 after `FIRST`, valid cp2 after `SECOND`; truncate the journal to cp1's receipt offset and flip a byte in the first event payload. Both checkpoint files remain unchanged. `recover_journal` rejects cp2's beyond-EOF watermark, replays from zero, stops at Checksum and reports `restoredseq=0 path=full-journal containsFIRST=false`. cp1 is intact and provides the `FIRST` model with an EOF tail. This is an explicit combined corruption/truncation diagnostic, not a claim that correctly fsynced files ordinarily lose bytes on this filesystem.

**Required repair:** evaluate candidates in descending order through all eligibility checks, including watermark/tail validation, before selecting one or falling back to full replay. Preserve diagnostics/originals. Add cases for newest invalid bytes, newest invalid offset, malformed sequence/boundary, older valid candidate and no valid candidate. **Fresh reproduction:** root added per-candidate journal-length eligibility and visible warnings. The same diagnostic now returns `restoredseq=1 path=checkpoint-tail containsFIRST=true`, with a warning naming rejected cp2. This exact beyond-EOF fallback case is resolved. More general sequence/boundary consistency is not proved by a file CRC alone; no natural writer counterexample was found in this slice.

### [HIGH] CP-06: inconsistent pending subparameters create a non-progressing parser loop

**Location:** `vendor/vte/src/params.rs:31`; `vendor/vte/src/ansi.rs:1870`.

`checkpoint_valid` checks that existing parameter groups partition `len`, and only checks `current_subparams <= len`. It does not require pending subparameters to describe the trailing group. A checkpoint with `len=2`, `subparams=[2,0,...]`, `current_subparams=1` passes validation. Advancing the final CSI `m` calls `Params::push`, changes a subparameter entry inside the first group and increments `len` to 3. Iteration then reaches index 2, where the count is zero: it emits an empty slice forever without advancing. The real SGR attribute loop continues forever. An imported historical checkpoint followed by this tail can hang host startup/recovery.

**Executed reproduction:** start a real `vte::ansi::Processor<LogicalTimeout>`, feed `ESC[1:2`, export its checkpoint to JSON, apply the three mutations above, import and advance by `m`. The importer accepts. The actual processor hangs after printing `accepted; advancing SGR tail`; an isolated owned subprocess was terminated after a 2-second diagnostic timeout. A separate finite direct-parser sample demonstrates `[[1,0], [], [], [], []]` from five iterator calls. This is a malformed checkpoint diagnostic, not a claim that ordinary live bytes create this impossible state. A correct CRC is not a substitute for semantic decoder validation.

**Required repair:** when `current_subparams != 0`, validate it is exactly the length of the trailing parameter group and that the group's start equals `len - current_subparams`. Reject inconsistent DTOs before executable parser state is created. Cover import plus real SGR tail, and valid pending colon sequences at every byte boundary. **Fresh verification:** root now validates `trailing_start == len - current_subparams` for nonzero pending subparameters. The same real-processor probe returns `invalid parser checkpoint` at import, cleanly before continuation, and the new VTE regression passes. The exact hang is resolved. Ignored interior array slots may retain valid stale values after `clear`; the repair correctly avoids assuming those slots must be zero.

## Verification actually executed by this reviewer

- Read local AGENTS, README, DESIGN, SPEC sections 7/8/10/12/13, handoff/map/versioning/worklog and ADRs 0001/0002.
- Baseline `cargo test -p pigade-terminal`: **9 passed**. Fresh corrected state.2 run: **10 passed**, including byte-boundary continuation and bounded shared-link frame encoding.
- Baseline `cargo test -p pigade-session-host checkpoint::tests --lib`: **1 passed**. Fresh corrected run: **2 passed**, including the older-valid-checkpoint/torn-journal regression.
- `cargo test --manifest-path vendor/vte/Cargo.toml --features serde checkpoint`: **2 passed**, including invalid pending UTF-8 and subparameter validation.
- `cargo test --offline --manifest-path vendor/alacritty_terminal/Cargo.toml --config 'patch.crates-io.vte.path="/home/afterburner/Projects/PigADE/vendor/vte"' term::checkpoint::tests -- --skip upstream_reference_corpus_composed_checkpoint_and_tail`: **8 passed**. The upstream reference/capture suite was deliberately not repeated by this reviewer; storage owns its full run.
- `cargo clippy -p pigade-terminal -p pigade-session-host --lib -- -D warnings`: **passed** on the corrected slice. `git diff --check`: passed.
- Initial standalone Alacritty test command without the explicit VTE patch failed with three E0599 missing checkpoint APIs because excluded vendor crates do not inherit the workspace patch; the explicit command above corrected the invocation. This is a verification setup correction, not a passing initial run.
- Standalone local-source Rust probes: original malformed parser panic; current malformed parser rejected; valid hyperlink amplification; malformed collection decode allocation; older-checkpoint fallback failure. The separate Cargo harness used the exact local vendor paths. Peak RSS was measured with Python `resource.getrusage(RUSAGE_CHILDREN).ru_maxrss`, one fresh subprocess per allocation measurement; these are scoped diagnostics, not release-performance measurements.
- No complete post-fix workspace baseline run, GPUI/renderer, platform/IME/accessibility, provider CLI matrix, VM reboot, 4×1 GiB/100k resize or real-time soak run was performed by this reviewer. Root owns integration checks. The reports must continue to distinguish these gaps from executed evidence.

## Reproduction appendix (state.1 baseline)

The following standalone harness is diagnostic evidence, not production code or an added dependency. Its duplicate envelope mirrors the state.1 wire format. Later schema revisions must deliberately update the probe or reject its bytes; do not weaken a decoder to accommodate it. Create a fresh temporary folder with `src/main.rs` below. The manifest uses the reviewed checkout's exact absolute local paths. `/tmp` was used only for functional corruption/allocation diagnostics, never durable-fsync measurements.

`Cargo.toml`:

```toml
[package]
name="checkpoint-review-probe"
version="0.1.0"
edition="2024"
[dependencies]
vte={path="/home/afterburner/Projects/PigADE/vendor/vte", features=["serde","ansi"]}
serde_json="1"
serde={version="1",features=["derive"]}
pigade-terminal={path="/home/afterburner/Projects/PigADE/crates/terminal"}
pigade-session-host={path="/home/afterburner/Projects/PigADE/crates/session-host"}
pigade-storage={path="/home/afterburner/Projects/PigADE/crates/storage"}
alacritty_terminal={path="/home/afterburner/Projects/PigADE/vendor/alacritty_terminal",features=["serde"]}
postcard={version="=1.1.3",default-features=false,features=["alloc","use-std"]}
uuid={version="1",features=["v4"]}
crc32fast="1"
[patch.crates-io]
vte={path="/home/afterburner/Projects/PigADE/vendor/vte"}
alacritty_terminal={path="/home/afterburner/Projects/PigADE/vendor/alacritty_terminal"}
```

`src/main.rs`:

```rust
use pigade_terminal::{Geometry,Terminal};
use serde::{Serialize,Deserialize};
#[derive(Clone,Serialize,Deserialize)]struct Logical{armed:bool}
#[derive(Serialize,Deserialize)]struct Envelope{
 schema:u32,profile:String,geometry:Geometry,title:String,
 term:alacritty_terminal::term::checkpoint::TermCheckpointV1,
 processor:vte::ansi::ProcessorCheckpointV1<Logical>,
}
fn main(){match std::env::args().nth(1).as_deref(){
 Some("fallback")=>{
 use pigade_storage::{Journal,JournalHeader,EventKind};
 let root=std::path::Path::new("/tmp/pigade-checkpoint-review/fallback");std::fs::create_dir_all(root).unwrap();
 let generation=uuid::Uuid::new_v4();
 let header=JournalHeader{session_id:uuid::Uuid::new_v4(),generation,profile:pigade_terminal::CORE_PROFILE.into(),initial_rows:8,initial_cols:40};
 let path=root.join(format!("{generation}.journal"));
 let mut journal=Journal::create(&path,header.clone()).unwrap();
 let mut terminal=Terminal::new(Geometry::new(8,40).unwrap(),false);
 let first=journal.append(EventKind::Output,b"FIRST").unwrap();terminal.output(b"FIRST");
 let save=|receipt:pigade_storage::Receipt,t:&Terminal|{
 let bytes=t.checkpoint().unwrap();let mut prefix=Vec::new();prefix.extend_from_slice(b"PIGCP001");prefix.extend_from_slice(header.session_id.as_bytes());prefix.extend_from_slice(header.generation.as_bytes());prefix.extend_from_slice(&receipt.sequence.to_le_bytes());prefix.extend_from_slice(&receipt.end_offset.to_le_bytes());prefix.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
 let mut crc=crc32fast::Hasher::new();crc.update(&prefix);crc.update(&bytes);prefix.extend_from_slice(&crc.finalize().to_le_bytes());prefix.extend_from_slice(&bytes);
 std::fs::write(root.join(format!("{generation}.{:020}.checkpoint",receipt.sequence)),prefix).unwrap();};
 save(first,&terminal);
 let second=journal.append(EventKind::Output,b"SECOND").unwrap();terminal.output(b"SECOND");save(second,&terminal);drop(journal);
 let mut data=std::fs::read(&path).unwrap();data.truncate(first.end_offset as usize);data[first.end_offset as usize-1]^=1;std::fs::write(&path,data).unwrap();
 let recovered=pigade_session_host::recover_journal(&path).unwrap();
 println!("restoredseq={} path={} containsFIRST={} error={:?}",recovered.sequence,recovered.recovery_path,recovered.frame.text().contains("FIRST"),recovered.storage_error);
 },
 Some("frame")|Some("hyperlink")=>{
 let mut terminal=Terminal::new(Geometry::new(24,80).unwrap(),false);
 let uri=format!("https://example.test/{}", "x".repeat(32768));
 let input=format!("\x1b]8;;{uri}\x07{}\x1b]8;;\x07", "A".repeat(4200));
 terminal.output(input.as_bytes());
 if std::env::args().nth(1).as_deref()==Some("frame"){let bytes=serde_json::to_vec(&terminal.frame()).unwrap();println!("frame_json_bytes={} exceeds_ipc_limit={}",bytes.len(),bytes.len()>8*1024*1024);return;}
 let bytes=terminal.checkpoint().unwrap();
 println!("raw_bytes={} checkpoint_bytes={} exceeds_host_limit={}",input.len(),bytes.len(),bytes.len()>128*1024*1024);
 },
 Some("make-large")=>{
 let source=Terminal::new(Geometry::new(8,40).unwrap(),false).checkpoint().unwrap();
 let mut snapshot:Envelope=postcard::from_bytes(&source).unwrap();
 let row=serde_json::from_str("{\"inner\":[],\"occ\":0}").unwrap();
 snapshot.term.grid.rows=vec![row;5_000_000];
 let bytes=postcard::to_allocvec(&snapshot).unwrap();
 std::fs::write("/tmp/pigade-checkpoint-review/large.checkpoint",&bytes).unwrap();
 println!("encoded {} bytes",bytes.len());
 },
 Some("load-large")=>{
 let bytes=std::fs::read("/tmp/pigade-checkpoint-review/large.checkpoint").unwrap();
 println!("input {} bytes",bytes.len());
 let result=Terminal::from_checkpoint(&bytes,false);
 println!("rejected={:?}",result.err());
 },
 _=>{
 let mut snapshot=serde_json::to_value(vte::Parser::new().export_checkpoint()).unwrap();
 snapshot["partial_utf8_len"]=serde_json::json!(3);
 snapshot["partial_utf8"]=serde_json::json!([65,66,67,0]);
 let snapshot=serde_json::from_value(snapshot).unwrap();
 match vte::Parser::from_checkpoint(snapshot){Ok(mut parser)=>{struct Sink;impl vte::Perform for Sink{} parser.advance(&mut Sink,b"X");},Err(e)=>println!("rejected at import: {e}")}
 }
}}
```

Commands:

```sh
cargo build --offline --manifest-path /tmp/pigade-checkpoint-review/Cargo.toml
/tmp/pigade-checkpoint-review/target/debug/checkpoint-review-probe
/tmp/pigade-checkpoint-review/target/debug/checkpoint-review-probe hyperlink
/tmp/pigade-checkpoint-review/target/debug/checkpoint-review-probe frame
/tmp/pigade-checkpoint-review/target/debug/checkpoint-review-probe make-large
/tmp/pigade-checkpoint-review/target/debug/checkpoint-review-probe load-large
/tmp/pigade-checkpoint-review/target/debug/checkpoint-review-probe fallback
```

For decode-only peak RSS, run a fresh Python process after `make-large`:

```python
import resource, subprocess
p = subprocess.run(['/tmp/pigade-checkpoint-review/target/debug/checkpoint-review-probe', 'load-large'], capture_output=True, text=True)
print(p.returncode, p.stdout, p.stderr)
print(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)
```

## Additional malformed-parameter processor reproducer

Use the same temporary Cargo manifest above, replace its `src/main.rs` with this source, build, then run the resulting executable under a 2-second process timeout. The baseline hangs after its first line; a repaired importer must return Err before continuation.

```rust
use serde::{Serialize,Deserialize};
#[derive(Default,Clone,Serialize,Deserialize)]struct Logical(bool);
impl vte::ansi::Timeout for Logical{fn set_timeout(&mut self,_:std::time::Duration){self.0=true;}fn clear_timeout(&mut self){self.0=false;}fn pending_timeout(&self)->bool{self.0}}
struct Sink;impl vte::ansi::Handler for Sink{}
fn main(){
 let mut parser=vte::ansi::Processor::<Logical>::new();parser.advance(&mut Sink,b"\x1b[1:2");
 let mut snapshot=serde_json::to_value(parser.export_checkpoint()).unwrap();
 snapshot["parser"]["params"]["len"]=serde_json::json!(2);
 snapshot["parser"]["params"]["current_subparams"]=serde_json::json!(1);
 snapshot["parser"]["params"]["subparams"]=serde_json::json!([2,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]);
 let snapshot=serde_json::from_value(snapshot).unwrap();
 let mut parser=vte::ansi::Processor::<Logical>::from_checkpoint(snapshot).expect("accepts inconsistent current_subparams");
 println!("accepted; advancing SGR tail");parser.advance(&mut Sink,b"m");println!("completed");
}
```

## Corrected state.2 diagnostic follow-up

The baseline appendix intentionally preserves the original wire representation. For the five-million-row probe against state.2, construct `alacritty_terminal::grid::checkpoint::RowCheckpointV2 { cells: Vec::new(), occupied: 0 }` instead of deserializing the old `Row<Cell>` JSON. The same envelope mirror refers to the locally patched `TermCheckpointV1` API type, which now carries schema 2 and pooled cells. This type name alone is not a wire version.

The hyperlink probe additionally imported the emitted bytes, asserted exact reexport equality, applied resize 12×61, normal→alternate→normal, REP and a text tail to original and restored instances, then compared their checkpoint bytes. A 4,097-mark `A + U+0301 × 4097` own-export/import probe succeeded with an 18,229-byte envelope. These demonstrate the repaired cases, not arbitrary-length grapheme or global memory certification.
