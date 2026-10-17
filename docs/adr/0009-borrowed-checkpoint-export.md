# ADR0009: borrow terminal state during checkpoint export

Status: implemented for 0.0.1-dev; global memory gate remains open.

The previous checkpoint path copied both grids into owned cell DTOs, copied
combining/title/link payloads into shared pools, then encoded that snapshot.
The 128 MiB encoded writer limit applied after these intermediate allocations.
Keeping the live model, published frames and an owned snapshot together raised
transient memory without contributing to checkpoint semantics.

PigADE now serializes a validated immutable `TermCheckpointRef` into the same
bounded Postcard writer. Rows and cells stream in the existing canonical ring
order; only logical columns are serialized. Extra identities and hyperlink
pointer/content deduplication keep their original pool order, including each
grid's current/saved cursor templates before the next grid. Large combining,
title and hyperlink payloads are borrowed. The bounded parser checkpoint still
has an owned copy. The owned DTO exporter remains available as an independent
compatibility oracle and for callers which require an owned value.

Schema2, CORE_PROFILE and valid checkpoint bytes remain unchanged. Tests compare
exact Postcard bytes across all 45 pinned reference recordings at three cuts,
hidden normal/alternate state, history/ring movement, unique cursor/row extras,
equal-content links, resize and excess row backing. Full envelope tests also
compare the former owned path at every fixture byte boundary, including partial
UTF-8, CSI, OSC and synchronized output. Source state stays unchanged. The
borrowed source validator must track the owned DTO validation contract; changes
to either require validation-parity and wire/continuation tests.

Import separately borrows the fixed envelope prefix before owned decoding.
It refuses incompatible schema/profile and a title above the existing 65,536
byte export bound. The prior importer admitted such titles but could not export
them again; the regression fails with the guard removed. The normal/alternate
grid decoders now share an 18,874,368-cell cap while retaining the old per-grid
16,777,216 cap. This covers every previously valid generic pair: one normal
grid plus an alternate viewport no larger than 512×4096. It does not impose the
host's tighter profile on the general-purpose vendor API.

Ten alternating fresh processes per export variant execute identical actual
model fixtures. Their maximum-RSS p50/p95/p99 in Linux KiB are
88,352/90,484/90,484 before and 80,036/82,168/82,168 after. See
[`checkpoint-workspace-paired-processes.json`](../../benchmarks/results/checkpoint-workspace-paired-processes.json)
and its source/binary references. These totals include model construction,
resize, census and export; they are not isolated checkpoint workspace or product
RSS. The first exploratory baseline is retained separately; the final baseline
was rebuilt with stable vendor sources before switching only the wrapper path.
Concurrent corpus/build work prevents an isolated timing acceptance claim.

Identity tables, Vec growth, encoded buffers, import DTOs, parser copies,
allocator overhead, UI frames and live combining/title/grid growth still need
admission accounting. This change removes specific deep copies; it does not
prove a whole-event allocation bound, storage quota or the SPEC700 MiB gate.
